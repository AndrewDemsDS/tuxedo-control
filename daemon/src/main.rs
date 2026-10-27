//! tuxedo-controld: fan-curve + performance daemon for TUXEDO Uniwill laptops.
//!
//! Owns the hardware (clears the 0x40 fan-ownership bit every tick so the EC can't steal the
//! fan), runs a temp→duty curve, and exposes a tiny line protocol on a Unix socket so the GUI
//! can read status and change profile / fan mode without touching the device or fighting us.
//!
//! Socket: /run/tuxedo-control.sock  (one request line -> one response line)
//!   STATUS            -> JSON {cpu_temp,gpu_temp,cpu_fan,gpu_fan,profile,manual,mode,curve_pct,
//!                        ...,ready}  (ready=false until the first loop tick, which can take
//!                        ~13 s after start on the reference board while the EC settles)
//!   PROFILE <name>    -> OK | ERR ...   (power_save|enthusiast|overboost)
//!   FANAUTO           -> OK             (clear manual override -> resume curve)
//!   FANMANUAL <0-100> -> OK             (hold a fixed duty)

use serde::Deserialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tuxedoio::{PerfProfile, TuxedoIo};

mod profiles;
use profiles::{Profile, Store};

const SOCK_PATH: &str = "/run/tuxedo-control.sock";
/// EMA window for temperature smoothing (anti fan-hunting): the curve reads an averaged temp
/// instead of the noisy instantaneous sensor. Higher = smoother but slower to react. At the
/// default 2 s poll, 10 samples is a ~20 s time constant: long enough to ride out the
/// 47 -> 60 C spikes a Ryzen package sensor shows under light load.
const TEMP_SMOOTH_N: f64 = 10.0;
/// Extra hysteresis (on top of `hysteresis_c`) before a spinning fan may stop. The step from
/// 0 to the curve's first duty is the audible one, so stopping waits for a few more degrees.
const FAN_OFF_EXTRA_C: i32 = 3;
/// Once the fan starts, it keeps its lowest curve duty for at least this long before stopping.
const FAN_MIN_ON: Duration = Duration::from_secs(60);
/// The safety floor reads a short EMA of the raw temperature: a Ryzen package sensor jumps
/// to 80-86 C for a few seconds on every load burst, and a floor on the raw value kicked the
/// fan to 45-60% and back on each one. 3 samples is ~6 s at the default poll.
const FLOOR_SMOOTH_N: f64 = 3.0;
/// At or above this raw temperature the floor skips the EMA and applies at once.
const FLOOR_RAW_C: i32 = 90;
/// Once the safety floor raises the fan, it holds for this long before it may drop.
const FLOOR_HOLD: Duration = Duration::from_secs(30);
/// Curve-driven decreases fall at most this many percent per tick, so the fan winds down
/// instead of stepping. Increases are not limited.
const FAN_DOWN_STEP_PCT: i32 = 2;
/// Deadband: don't rewrite fan duty for curve-driven changes smaller than this many percent.
/// Stops the fan oscillating on small temperature wobble; the safety floor still overrides it.
const FAN_DEADBAND_PCT: i32 = 3;
/// Consecutive implausible CPU temperature reads (error, <= 0 or >= 120 C) before the loop
/// stops trusting the EC and hands the fan back to its automatic control.
const MAX_BAD_TEMP_READS: u32 = 3;
/// Socket clients are served one at a time, so a client that connects and never finishes its
/// line must not hold the socket thread: give up on it after this long.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest request line accepted (a SAVEPROFILE JSON is well under 1 KiB).
const MAX_REQUEST: u64 = 64 * 1024;

/// A CPU temperature the curve may act on. 0 is what a failed or raced EC read returns, and
/// acting on it would let the curve switch the fan off while the CPU is hot.
fn plausible_temp(t: i32) -> bool {
    t > 0 && t < 120
}

/// Hand the fan back to the driver and re-apply the perf profile. tuxedo_io programs the EC's
/// custom fan table only on the first fan write after load or after FANAUTO (its
/// `fans_initialized` flag). That flag survives sleep and a SIGKILLed daemon, while hibernation
/// resets the EC (fan table off, 0x0751 back to 0), so later duty writes were silently ignored
/// and the EC's own control fought ours. FANAUTO clears the flag; the next write re-inits.
fn reinit_ec(d: &TuxedoIo, perf: Option<PerfProfile>) {
    let _ = d.restore_auto();
    if let Some(p) = perf {
        let _ = d.set_perf(p);
    }
}

/// Total time the system has spent suspended: CLOCK_BOOTTIME counts sleep, CLOCK_MONOTONIC
/// does not, and both are slewed alike, so the gap only grows across a suspend or hibernate.
fn suspended_total() -> Duration {
    fn clock(id: libc::clockid_t) -> Duration {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(id, &mut ts) };
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
    let mono = clock(libc::CLOCK_MONOTONIC);
    clock(libc::CLOCK_BOOTTIME).saturating_sub(mono)
}

/// Sleep for `d`, waking early once `term` is set. A plain sleep of the poll interval delayed
/// SIGTERM (and so the stop before suspend) by up to `poll_seconds`, and a long interval could
/// outlast `TimeoutStopSec`, so systemd killed the daemon before it handed the fan back.
fn sleep_unless(term: &AtomicBool, d: Duration) {
    let end = Instant::now() + d;
    while !term.load(Ordering::Relaxed) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// Apply a profile's perf mode + (optional) keyboard backlight + charge profile.
/// The control loop reads the fan curve live from the active profile.
fn apply_profile(dev: &Arc<Mutex<TuxedoIo>>, shared: &Arc<Mutex<Shared>>, p: &Profile) {
    // On an unsupported board we never write to the EC (perf lives in the same 0x0751
    // register as fan ownership). Keyboard backlight / charging are sysfs and stay allowed.
    let read_only = shared.lock().unwrap().read_only;
    if !read_only {
        if let Some(pp) = PerfProfile::from_name(&p.perf) {
            let _ = dev.lock().unwrap().set_perf(pp);
            shared.lock().unwrap().profile = pp.as_id().into();
        }
    }
    if let Some(b) = p.kbd {
        let dir = shared.lock().unwrap().kbd_dir.clone();
        if let Some(d) = dir {
            let _ = kbd_write(&d, b);
            shared.lock().unwrap().kbd_cur = b;
        }
    }
    if let Some(ref c) = p.charge {
        if chg_present() && chg_set(c).is_ok() {
            shared.lock().unwrap().charge = c.clone();
        }
    }
}

#[derive(Deserialize)]
struct Config {
    #[serde(default = "d_poll")]
    poll_seconds: u64,
    #[serde(default = "d_hyst")]
    hysteresis_c: i32,
    #[serde(default)]
    profile: Option<String>, // built-in/profile name to activate at start
    #[serde(default)]
    perf_profile: Option<String>,
    #[serde(default)]
    kbd_brightness: Option<i32>,
    #[serde(default)]
    charge_profile: Option<String>,
    #[serde(default = "d_curve")]
    curve: Vec<(i32, i32)>,
}
fn d_poll() -> u64 {
    2
}
fn d_hyst() -> i32 {
    3
}
fn d_curve() -> Vec<(i32, i32)> {
    vec![
        (25, 0),
        (50, 0),
        (62, 24),
        (68, 36),
        (72, 44),
        (80, 60),
        (86, 72),
        (90, 100),
    ]
}
impl Default for Config {
    fn default() -> Self {
        Config {
            poll_seconds: d_poll(),
            hysteresis_c: d_hyst(),
            profile: None,
            perf_profile: None,
            kbd_brightness: None,
            charge_profile: None,
            curve: d_curve(),
        }
    }
}

/// The control loop and socket server share this.
#[derive(Default)]
struct Shared {
    cpu_temp: i32,
    gpu_temp: i32,
    cpu_fan: i32,
    gpu_fan: i32,
    mode: i32,
    curve_pct: i32,      // what the curve asks for
    manual: Option<i32>, // Some(pct) => hold a fixed duty; None => follow the curve
    profile: String,
    kbd_dir: Option<String>, // /sys/class/leds/<...kbd_backlight> if present
    kbd_max: i32,
    kbd_cur: i32,
    charge: String,
    charges: String, // current + space-separated available
    model_id: i32,
    read_only: bool, // unsupported model: no fan/EC writes, status only
    ready: bool,     // false until the first control-loop tick filled the cache
}

// ---- Keyboard backlight (sysfs led) ----
/// The leds class dir. Overridable via `TUXEDO_LEDS_DIR` so tests can point it at a tempdir;
/// production always uses `/sys/class/leds`.
fn leds_dir() -> String {
    std::env::var("TUXEDO_LEDS_DIR").unwrap_or_else(|_| "/sys/class/leds".to_string())
}
fn kbd_find() -> Option<String> {
    let dir = std::fs::read_dir(leds_dir()).ok()?;
    for e in dir.flatten() {
        let name = e.file_name();
        if name.to_string_lossy().contains("kbd_backlight") {
            return Some(e.path().to_string_lossy().into_owned());
        }
    }
    None
}
fn kbd_read(dir: &str, attr: &str) -> i32 {
    std::fs::read_to_string(format!("{dir}/{attr}"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(-1)
}
fn kbd_write(dir: &str, v: i32) -> std::io::Result<()> {
    std::fs::write(format!("{dir}/brightness"), v.max(0).to_string())
}

// ---- Battery charging profile (sysfs; stationary / balanced / high_capacity) ----
const CHG_DIR: &str = "/sys/devices/platform/tuxedo_keyboard/charging_profile";
/// The charging-profile sysfs dir. Overridable via `TUXEDO_CHG_DIR` so tests can point it at a
/// tempdir; production always uses the default. (Set it before any chg_* call in a test.)
fn chg_dir() -> String {
    std::env::var("TUXEDO_CHG_DIR").unwrap_or_else(|_| CHG_DIR.to_string())
}
fn chg_present() -> bool {
    std::path::Path::new(&format!("{}/charging_profile", chg_dir())).exists()
}
fn chg_get() -> String {
    std::fs::read_to_string(format!("{}/charging_profile", chg_dir()))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
fn chg_avail() -> String {
    std::fs::read_to_string(format!("{}/charging_profiles_available", chg_dir()))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
fn chg_set(v: &str) -> std::io::Result<()> {
    if !["high_capacity", "balanced", "stationary"].contains(&v) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "bad profile",
        ));
    }
    std::fs::write(format!("{}/charging_profile", chg_dir()), v)
}

/// The curve's duty for a smoothed temperature, given the duty commanded last tick.
/// Rising follows the curve; falling lags by `hyst` degrees. Stopping a spinning fan needs
/// `hyst + FAN_OFF_EXTRA_C` degrees of margin and `FAN_MIN_ON` of run time; until then the
/// fan holds the curve's lowest nonzero duty, so it does not start and stop every minute.
fn curve_duty(curve: &[(i32, i32)], temp: i32, prev: i32, hyst: i32, on_for: Duration) -> i32 {
    let prev = prev.max(0);
    let up = interp(curve, temp);
    if up >= prev {
        return up;
    }
    let down = interp(curve, temp.saturating_add(hyst)).min(prev);
    if down > 0 || prev == 0 {
        return down;
    }
    let off_temp = temp.saturating_add(hyst).saturating_add(FAN_OFF_EXTRA_C);
    if on_for >= FAN_MIN_ON && interp(curve, off_temp) == 0 {
        return 0;
    }
    let floor = curve
        .iter()
        .map(|&(_, d)| d)
        .filter(|&d| d > 0)
        .min()
        .unwrap_or(0);
    floor.min(prev)
}

/// The duty to command this tick, from the curve's duty, the previous command, the curve's
/// lowest nonzero duty and an optional manual override. A manual duty applies exactly.
/// Otherwise changes under `FAN_DEADBAND_PCT` are ignored and decreases fall by at most
/// `FAN_DOWN_STEP_PCT` per tick, until the next step would drop below `curve_min`: then a
/// curve duty of 0 (the fan stopping, which `curve_duty` already gated) applies directly.
fn next_duty(curve_target: i32, prev: i32, curve_min: i32, manual: Option<i32>) -> i32 {
    if let Some(m) = manual {
        return m;
    }
    let prev = prev.max(0);
    if (curve_target - prev).abs() < FAN_DEADBAND_PCT {
        return prev;
    }
    if curve_target >= prev {
        return curve_target;
    }
    let stepped = prev - FAN_DOWN_STEP_PCT;
    if curve_target == 0 && stepped < curve_min {
        return 0;
    }
    curve_target.max(stepped)
}

/// Safety-floor state: the floor rises at once and holds for `FLOOR_HOLD` before it may fall.
#[derive(Default)]
struct FloorHold {
    duty: i32,
    since: Option<Instant>,
}

impl FloorHold {
    fn update(&mut self, floor: i32, now: Instant) -> i32 {
        let expired = self
            .since
            .is_none_or(|t| now.duration_since(t) >= FLOOR_HOLD);
        if floor >= self.duty || expired {
            self.duty = floor;
            self.since = (floor > 0).then_some(now);
        }
        self.duty
    }
}

/// Duty % for `temp`, clamped to 0..=100. Curves arrive unchecked over the socket and from TCC
/// imports. In i32 a large point overflowed the slope math (a panic in debug builds, a wrapped
/// and possibly negative duty, i.e. fan off, in release); i128 holds any pair of i32 points.
fn interp(curve: &[(i32, i32)], temp: i32) -> i32 {
    interp_raw(curve, temp).clamp(0, 100)
}

fn interp_raw(curve: &[(i32, i32)], temp: i32) -> i32 {
    if curve.is_empty() {
        return 0;
    }
    if temp <= curve[0].0 {
        return curve[0].1;
    }
    if temp >= curve[curve.len() - 1].0 {
        return curve[curve.len() - 1].1;
    }
    for w in curve.windows(2) {
        let ((t0, d0), (t1, d1)) = (w[0], w[1]);
        if temp >= t0 && temp <= t1 {
            return if t1 == t0 {
                d1
            } else {
                let (t0, d0, t1, d1, t) =
                    (t0 as i128, d0 as i128, t1 as i128, d1 as i128, temp as i128);
                (d0 + (d1 - d0) * (t - t0) / (t1 - t0)).clamp(0, 100) as i32
            };
        }
    }
    curve[curve.len() - 1].1
}

fn load_config() -> Config {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/tuxedo-control/config.json".into());
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            eprintln!("config parse {path}: {e}; defaults");
            Config::default()
        }),
        Err(_) => {
            eprintln!("no config at {path}; built-in quiet defaults");
            Config::default()
        }
    }
}

fn handle_client(
    stream: UnixStream,
    dev: &Arc<Mutex<TuxedoIo>>,
    shared: &Arc<Mutex<Shared>>,
    store: &Arc<Mutex<Store>>,
) {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    })
    .take(MAX_REQUEST);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut w = stream;
    let mut parts = line.split_whitespace();
    // The argument after the command word, for names that contain spaces.
    let rest = line
        .split_once(char::is_whitespace)
        .map(|(_, r)| r.trim())
        .unwrap_or("");
    let resp: String = match parts.next() {
        Some("STATUS") => {
            // Read ONLY the cache the loop maintains: instant, no device/EC access, no contention.
            let s = shared.lock().unwrap();
            format!("{{\"cpu_temp\":{},\"gpu_temp\":{},\"cpu_fan\":{},\"gpu_fan\":{},\"mode\":{},\"curve_pct\":{},\"manual\":{},\"profile\":\"{}\",\"kbd\":{},\"kbd_max\":{},\"charge\":\"{}\",\"charges\":\"{}\",\"model_id\":{},\"read_only\":{},\"ready\":{}}}",
                s.cpu_temp, s.gpu_temp, s.cpu_fan, s.gpu_fan, s.mode, s.curve_pct,
                s.manual.map(|m| m.to_string()).unwrap_or_else(|| "null".into()), s.profile, s.kbd_cur, s.kbd_max,
                s.charge, s.charges, s.model_id, s.read_only, s.ready)
        }
        // Reject EC writes on unsupported boards (perf shares the 0x0751 register with fan).
        Some("PROFILE") if shared.lock().unwrap().read_only => {
            "ERR read-only (unsupported model)".into()
        }
        Some("PROFILE") => match parts.next().and_then(PerfProfile::from_name) {
            // Serialise EC access through the one device handle (concurrent ioctls corrupt reads).
            Some(p) => {
                let _ = dev.lock().unwrap().set_perf(p);
                shared.lock().unwrap().profile = p.as_id().into();
                "OK".into()
            }
            None => "ERR bad profile".into(),
        },
        Some("KBDSET") => match parts.next().and_then(|s| s.parse::<i32>().ok()) {
            Some(v) => {
                let dir = shared.lock().unwrap().kbd_dir.clone();
                match dir {
                    Some(d) => {
                        if kbd_write(&d, v).is_ok() {
                            shared.lock().unwrap().kbd_cur = v;
                            "OK".into()
                        } else {
                            "ERR write".into()
                        }
                    }
                    None => "ERR no kbd backlight".into(),
                }
            }
            None => "ERR bad value".into(),
        },
        Some("CHARGE") => match parts.next() {
            Some(v) => {
                if chg_set(v).is_ok() {
                    shared.lock().unwrap().charge = v.to_string();
                    "OK".into()
                } else {
                    "ERR charge set".into()
                }
            }
            None => "ERR bad value".into(),
        },
        // ---- named profiles ----
        Some("LISTPROFILES") => {
            serde_json::to_string(&*store.lock().unwrap()).unwrap_or_else(|_| "ERR".into())
        }
        Some("ACTIVATE") if !rest.is_empty() => {
            let prof = store.lock().unwrap().get(rest).cloned();
            match prof {
                Some(p) => {
                    apply_profile(dev, shared, &p);
                    let mut s = store.lock().unwrap();
                    s.active = rest.to_string();
                    let _ = profiles::save(&s);
                    "OK".into()
                }
                None => "ERR no such profile".into(),
            }
        }
        Some("SETDEFAULT") if !rest.is_empty() => {
            let mut s = store.lock().unwrap();
            if s.get(rest).is_some() {
                s.default = rest.to_string();
                let _ = profiles::save(&s);
                "OK".into()
            } else {
                "ERR no such profile".into()
            }
        }
        Some("DELPROFILE") if !rest.is_empty() => {
            if profiles::is_builtin(rest) {
                "ERR built-in profiles cannot be deleted".into()
            } else {
                let mut s = store.lock().unwrap();
                s.profiles.retain(|p| p.name != rest);
                if s.active == rest {
                    s.active = s.default.clone();
                }
                let _ = profiles::save(&s);
                "OK".into()
            }
        }
        Some("SAVEPROFILE") => {
            // rest of the line is a JSON Profile object
            let json = rest;
            match serde_json::from_str::<Profile>(json) {
                Ok(p) => {
                    let mut s = store.lock().unwrap();
                    s.profiles.retain(|x| x.name != p.name);
                    s.profiles.push(p);
                    let _ = profiles::save(&s);
                    "OK".into()
                }
                Err(e) => format!("ERR {e}"),
            }
        }
        Some("IMPORTTCC") if !rest.is_empty() => match std::fs::read_to_string(rest) {
            Ok(txt) => match profiles::import_tcc(&txt) {
                Ok(imported) => {
                    let n = imported.len();
                    let mut s = store.lock().unwrap();
                    for p in imported {
                        s.profiles.retain(|x| x.name != p.name);
                        s.profiles.push(p);
                    }
                    let _ = profiles::save(&s);
                    format!("OK imported {n}")
                }
                Err(e) => format!("ERR {e}"),
            },
            Err(e) => format!("ERR read {e}"),
        },
        Some("FANAUTO") => {
            shared.lock().unwrap().manual = None;
            "OK".into()
        }
        Some("FANMANUAL") if shared.lock().unwrap().read_only => {
            "ERR read-only (unsupported model)".into()
        }
        Some("FANMANUAL") => match parts.next().and_then(|s| s.parse::<i32>().ok()) {
            Some(p) => {
                shared.lock().unwrap().manual = Some(p.clamp(0, 100));
                "OK".into()
            }
            None => "ERR bad pct".into(),
        },
        _ => "ERR unknown".into(),
    };
    let _ = writeln!(w, "{resp}");
}

fn main() {
    let cfg = load_config();
    // ONE device handle behind a mutex. Concurrent opens/ioctls on the Uniwill EC
    // interface corrupt reads (temps came back 0), so we serialise every access here.
    let dev = match TuxedoIo::open() {
        Ok(d) => Arc::new(Mutex::new(d)),
        Err(e) => {
            eprintln!("open /dev/tuxedo_io: {e}");
            std::process::exit(1);
        }
    };
    let model_id;
    let read_only;
    {
        let d = dev.lock().unwrap();
        if !d.is_uniwill().unwrap_or(false) {
            eprintln!("not Uniwill, refusing");
            std::process::exit(1);
        }
        // The library gate (set in TuxedoIo::open from the model id) is the source of truth:
        // an unvalidated board refuses EC writes, so we run READ-ONLY (status only). Reading
        // model id here is just for logging/STATUS and is independent of the gate, so a failed
        // capability probe can never demote a validated board.
        model_id = d.model_id().unwrap_or(0);
        read_only = !d.write_allowed();
        if read_only {
            eprintln!(
                "tuxedo-controld: tuxedo_io {}, model {model_id:#x} UNSUPPORTED -> READ-ONLY \
                 (no fan/EC writes). Validate with the prober and add it to KNOWN_MODELS; \
                 see docs/model-gating.md.",
                d.version().unwrap_or_default()
            );
        } else {
            eprintln!(
                "tuxedo-controld: tuxedo_io {}, model {model_id:#x} ({}), poll {}s, hyst {}C",
                d.version().unwrap_or_default(),
                tuxedoio::known_model(model_id)
                    .map(|m| m.name)
                    .unwrap_or("validated"),
                cfg.poll_seconds,
                cfg.hysteresis_c
            );
        }
        // Best-effort capability log (must not affect gating).
        eprintln!(
            "caps: fans_off={} min_speed={}% profs_avail={}",
            d.fans_off_available().unwrap_or(false),
            d.fans_min_speed().unwrap_or(25),
            d.profs_available().unwrap_or(0)
        );
    }

    let shared = Arc::new(Mutex::new(Shared::default()));
    {
        let mut s = shared.lock().unwrap();
        s.model_id = model_id;
        s.read_only = read_only;
    }
    // Discover keyboard backlight + charging support first (apply_profile uses them).
    if let Some(dir) = kbd_find() {
        let max = kbd_read(&dir, "max_brightness");
        eprintln!("keyboard backlight at {dir} (max {max})");
        let mut s = shared.lock().unwrap();
        s.kbd_dir = Some(dir);
        s.kbd_max = max;
    } else {
        eprintln!("no keyboard backlight led found");
    }
    if chg_present() {
        eprintln!("charging profiles: {} (current {})", chg_avail(), chg_get());
        let mut s = shared.lock().unwrap();
        s.charges = chg_avail();
        s.charge = chg_get();
    } else {
        eprintln!("no charging-profile support");
    }

    // ---- Named profiles (TCC-style). The active profile drives perf/fan/kbd/charge. ----
    let fresh = !std::path::Path::new(profiles::STORE_PATH).exists();
    let store = {
        let mut s = profiles::load();
        // First run: fold the declarative config (curve/perf/kbd/charge) into a "Configured"
        // profile and make it active, so an existing NixOS config keeps working as a profile.
        if fresh && cfg.perf_profile.is_some() {
            let p = Profile {
                name: "Configured".into(),
                perf: cfg.perf_profile.clone().unwrap(),
                curve: cfg.curve.clone(),
                kbd: cfg.kbd_brightness,
                charge: cfg.charge_profile.clone(),
            };
            s.profiles.insert(0, p);
            s.active = "Configured".into();
            s.default = "Configured".into();
            let _ = profiles::save(&s);
        }
        Arc::new(Mutex::new(s))
    };
    // Honour a configured active profile name (e.g. the module's defaultProfile).
    if let Some(ref name) = cfg.profile {
        let mut s = store.lock().unwrap();
        if s.get(name).is_some() && s.active != *name {
            s.active = name.clone();
            let _ = profiles::save(&s);
        }
    }
    {
        let active = store.lock().unwrap().active_profile().cloned();
        match active {
            Some(p) => {
                eprintln!("active profile: {} (perf {})", p.name, p.perf);
                apply_profile(&dev, &shared, &p);
            }
            None => eprintln!("no active profile"),
        }
    }

    // Declarative peripheral state: re-assert the configured keyboard backlight / charging
    // profile on every startup, independent of the named-profile store (which only carries
    // these when a "Configured" profile was seeded on first run). This makes the module's
    // keyboard.backlight / charging options take effect on each boot, not just a fresh store.
    if let Some(b) = cfg.kbd_brightness {
        let dir = shared.lock().unwrap().kbd_dir.clone();
        if let Some(d) = dir {
            let _ = kbd_write(&d, b);
            shared.lock().unwrap().kbd_cur = b;
        }
    }
    if let Some(ref c) = cfg.charge_profile {
        if chg_present() && chg_set(c).is_ok() {
            shared.lock().unwrap().charge = c.clone();
        }
    }

    // Socket server: shares the one device handle (locks it only for PROFILE).
    {
        let _ = std::fs::remove_file(SOCK_PATH);
        match UnixListener::bind(SOCK_PATH) {
            Ok(listener) => {
                // World-accessible so the user's GUI can connect (local single-user laptop).
                let _ = std::fs::set_permissions(
                    SOCK_PATH,
                    std::os::unix::fs::PermissionsExt::from_mode(0o666),
                );
                let (sh, dv, st) = (shared.clone(), dev.clone(), store.clone());
                std::thread::spawn(move || {
                    for stream in listener.incoming().flatten() {
                        handle_client(stream, &dv, &sh, &st);
                    }
                });
                eprintln!("control socket at {SOCK_PATH}");
            }
            Err(e) => {
                eprintln!("socket bind failed ({e}); GUI control unavailable, fan loop continues")
            }
        }
    }

    let term = Arc::new(AtomicBool::new(false));
    install_signals(term.clone());

    // A previous instance that was killed (or a resume the sleep unit did not cover) can leave
    // the driver believing its fan table is still programmed. Reset it before the first write.
    if !read_only {
        dev.lock().unwrap().restore_auto().ok();
    }
    let mut last_suspended = suspended_total();

    let mut commanded: i32 = -1;
    let mut smoothed_temp: f64 = -1.0; // EMA state; <0 = unseeded (first tick)
    let mut floor_temp: f64 = -1.0; // fast EMA for the safety floor, same convention
    let mut floor_hold = FloorHold::default();
    let mut fan_on_since: Option<Instant> = None; // when the fan last started from 0
    let mut bad_temp_reads: u32 = 0;
    let mut ec_fallback = false; // true while the fan is handed back to the EC
    let debug = std::env::var_os("TUXEDO_DEBUG").is_some();
    // poll_seconds = 0 would spin on the EC with no pause at all.
    let poll = Duration::from_secs(cfg.poll_seconds.max(1));
    while !term.load(Ordering::Relaxed) {
        // Resumed from sleep while running (no tuxedo-control-sleep.service, or it raced):
        // the EC may have been reset under us, so redo the setup a fresh start would do.
        let suspended = suspended_total();
        if suspended.saturating_sub(last_suspended) > Duration::from_secs(1) && !read_only {
            eprintln!("resumed from sleep: re-initialising EC fan control and perf profile");
            let perf = PerfProfile::from_name(&shared.lock().unwrap().profile);
            reinit_ec(&dev.lock().unwrap(), perf);
            commanded = -1;
            smoothed_temp = -1.0;
            floor_temp = -1.0;
            floor_hold = FloorHold::default();
            fan_on_since = None;
        }
        last_suspended = suspended;
        // Hold the device lock for the whole tick's ioctls (reads + fan write), then release.
        let (ct, gt, cpu_fan, gpu_fan, mode);
        let curve_target;
        // The active profile's fan curve (falls back to the config curve if none).
        let curve = store
            .lock()
            .unwrap()
            .active_profile()
            .map(|p| p.curve.clone())
            .unwrap_or_else(|| cfg.curve.clone());
        {
            let d = dev.lock().unwrap();
            ct = d.cpu_temp().unwrap_or(0);
            gt = d.gpu_temp().unwrap_or(0);
            if debug {
                eprintln!("tick: cpu_temp={ct} gpu_temp={gt}");
            }
            // Never steer the fan from an implausible reading. A few in a row means the EC
            // interface is not answering: give the fan back to the EC instead of guessing.
            if !plausible_temp(ct) {
                bad_temp_reads += 1;
                if bad_temp_reads >= MAX_BAD_TEMP_READS && !ec_fallback && !read_only {
                    eprintln!(
                        "{bad_temp_reads} implausible CPU temperature reads (last {ct}C): \
                         handing the fan to the EC until reads recover"
                    );
                    let _ = d.restore_auto();
                    ec_fallback = true;
                }
            }
        }
        if !plausible_temp(ct) {
            {
                let mut s = shared.lock().unwrap();
                s.cpu_temp = ct;
                s.gpu_temp = gt;
            }
            sleep_unless(&term, poll);
            continue;
        }
        if ec_fallback {
            eprintln!("CPU temperature reads recovered ({ct}C): resuming the fan curve");
            ec_fallback = false;
            commanded = -1;
            smoothed_temp = -1.0;
            floor_temp = -1.0;
            floor_hold = FloorHold::default();
            fan_on_since = None;
        }
        bad_temp_reads = 0;
        // The fan write below can take many seconds (the first one after start or after
        // FANAUTO re-initialises the EC fan tables), so don't start it once asked to stop.
        if term.load(Ordering::Relaxed) {
            break;
        }
        {
            let d = dev.lock().unwrap();
            let temp_raw = ct.max(gt);
            // EMA-smooth the temperature so the fan follows the trend, not every sensor spike.
            // This is the main fix for audible up/down fan hunting. First tick seeds directly.
            // Float state: an integer EMA truncates and can sit degrees below a steady reading.
            smoothed_temp = if smoothed_temp < 0.0 {
                f64::from(temp_raw)
            } else {
                (smoothed_temp * (TEMP_SMOOTH_N - 1.0) + f64::from(temp_raw)) / TEMP_SMOOTH_N
            };
            let temp = smoothed_temp.round() as i32;
            floor_temp = if floor_temp < 0.0 {
                f64::from(temp_raw)
            } else {
                (floor_temp * (FLOOR_SMOOTH_N - 1.0) + f64::from(temp_raw)) / FLOOR_SMOOTH_N
            };
            let on_for = fan_on_since.map_or(Duration::ZERO, |t| t.elapsed());
            curve_target = curve_duty(&curve, temp, commanded, cfg.hysteresis_c, on_for);

            let manual = shared.lock().unwrap().manual;
            let curve_min = curve
                .iter()
                .map(|&(_, d)| d)
                .filter(|&d| d > 0)
                .min()
                .unwrap_or(0);
            let held = next_duty(curve_target, commanded, curve_min, manual);
            // Safety net on a fast EMA of the temperature (on the raw value from FLOOR_RAW_C):
            // never sit below the safety floor, whatever the curve, deadband or slew say.
            let floor_c = if temp_raw >= FLOOR_RAW_C {
                temp_raw
            } else {
                floor_temp.round() as i32
            };
            let floor = floor_hold.update(profiles::safety_floor(floor_c), Instant::now());
            let target = held.max(floor);
            // Read-only on unsupported boards: report the curve target but never drive the EC.
            if !read_only && d.set_fan_pct(target).is_ok() && target != commanded {
                eprintln!(
                    "temp {temp_raw}C (avg {temp}C) -> fan {target}%{}",
                    if manual.is_some() { " (manual)" } else { "" }
                );
            }
            if target == 0 {
                fan_on_since = None;
            } else if commanded <= 0 {
                fan_on_since = Some(Instant::now());
            }
            commanded = target;
            cpu_fan = d.cpu_fan_pct().unwrap_or(0);
            gpu_fan = d.gpu_fan_pct().unwrap_or(0);
            mode = d.mode().unwrap_or(0);
        }
        // Cache the slow EC/sysfs reads here (on the loop) so STATUS stays instant.
        let kbd_cur = {
            let s = shared.lock().unwrap();
            s.kbd_dir
                .as_deref()
                .map(|d| kbd_read(d, "brightness"))
                .unwrap_or(-1)
        };
        let charge = if chg_present() {
            chg_get()
        } else {
            String::new()
        };
        {
            let mut s = shared.lock().unwrap();
            s.cpu_temp = ct;
            s.gpu_temp = gt;
            s.cpu_fan = cpu_fan;
            s.gpu_fan = gpu_fan;
            s.mode = mode;
            s.curve_pct = curve_target;
            s.kbd_cur = kbd_cur;
            s.charge = charge;
            s.ready = true;
        }
        sleep_unless(&term, poll);
    }

    // Only hand the fan back to the EC if we ever took it (read-only never touched it).
    if !read_only {
        eprintln!("shutting down, restoring EC auto fan");
        let _ = dev.lock().unwrap().restore_auto();
    } else {
        eprintln!("shutting down (read-only; EC fan untouched)");
    }
    let _ = std::fs::remove_file(SOCK_PATH);
}

static TERM: AtomicBool = AtomicBool::new(false);
extern "C" fn on_sig(_: libc::c_int) {
    TERM.store(true, Ordering::Relaxed);
}
fn install_signals(term: Arc<AtomicBool>) {
    unsafe {
        let h = on_sig as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
    std::thread::spawn(move || loop {
        if TERM.load(Ordering::Relaxed) {
            term.store(true, Ordering::Relaxed);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir for one test fn (process id keeps it unique across concurrent
    /// `cargo test` runs; the `tag` keeps each test fn's dir distinct within a run).
    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tuxedo-test-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- curve_duty (fan on/off hysteresis and minimum run time) ----

    const QUIET: [(i32, i32); 6] = [(0, 0), (50, 0), (51, 20), (70, 33), (85, 70), (100, 100)];
    const LONG: Duration = Duration::from_secs(120);

    #[test]
    fn curve_duty_starts_the_fan_on_the_curve() {
        assert_eq!(curve_duty(&QUIET, 51, 0, 3, Duration::ZERO), 20);
        assert_eq!(curve_duty(&QUIET, 50, 0, 3, Duration::ZERO), 0);
    }

    #[test]
    fn curve_duty_holds_the_lowest_duty_during_min_run_time() {
        // Cool enough to stop, but the fan only just started.
        assert_eq!(curve_duty(&QUIET, 40, 20, 3, Duration::from_secs(10)), 20);
        // Coming down from a higher duty, it settles on the floor rather than stopping.
        assert_eq!(curve_duty(&QUIET, 40, 33, 3, Duration::from_secs(10)), 20);
    }

    #[test]
    fn curve_duty_needs_the_extra_margin_to_stop() {
        // 47 C: plain hysteresis would stop it (47 + 3 = 50 -> 0), the extra 3 C does not.
        assert_eq!(curve_duty(&QUIET, 47, 20, 3, LONG), 20);
        assert_eq!(curve_duty(&QUIET, 45, 20, 3, LONG), 20);
        assert_eq!(curve_duty(&QUIET, 44, 20, 3, LONG), 0);
    }

    #[test]
    fn curve_duty_falls_with_hysteresis_while_running() {
        // 60 C on the way down reads the curve at 63 C, never above the previous duty.
        assert_eq!(curve_duty(&QUIET, 60, 33, 3, LONG), interp(&QUIET, 63));
        assert_eq!(curve_duty(&QUIET, 55, 24, 3, LONG), 24);
    }

    #[test]
    fn curve_duty_unseeded_prev_behaves_like_off() {
        assert_eq!(curve_duty(&QUIET, 40, -1, 3, Duration::ZERO), 0);
        assert_eq!(
            curve_duty(&QUIET, 60, -1, 3, Duration::ZERO),
            interp(&QUIET, 60)
        );
    }

    // ---- next_duty (deadband and down slew) ----

    #[test]
    fn next_duty_rises_at_once_and_ignores_small_changes() {
        assert_eq!(next_duty(60, 21, 20, None), 60);
        assert_eq!(next_duty(23, 21, 20, None), 21);
        assert_eq!(next_duty(19, 21, 20, None), 21);
    }

    #[test]
    fn next_duty_winds_down_in_steps() {
        assert_eq!(next_duty(30, 60, 20, None), 58);
        assert_eq!(next_duty(30, 33, 20, None), 31);
        // The fan stops only once the slew reaches the curve's lowest duty.
        assert_eq!(next_duty(0, 33, 20, None), 31);
        assert_eq!(next_duty(0, 21, 20, None), 0);
    }

    #[test]
    fn next_duty_applies_manual_exactly() {
        assert_eq!(next_duty(60, 21, 20, Some(22)), 22);
        assert_eq!(next_duty(60, 80, 20, Some(0)), 0);
    }

    #[test]
    fn next_duty_unseeded_prev_starts_on_the_curve() {
        assert_eq!(next_duty(26, -1, 20, None), 26);
        assert_eq!(next_duty(0, -1, 20, None), 0);
    }

    // ---- FloorHold (safety floor rises at once, falls after FLOOR_HOLD) ----

    #[test]
    fn floor_hold_rises_and_holds() {
        let t0 = Instant::now();
        let mut h = FloorHold::default();
        assert_eq!(h.update(0, t0), 0);
        assert_eq!(h.update(45, t0), 45);
        assert_eq!(h.update(60, t0 + Duration::from_secs(2)), 60);
        // A cooler reading inside the hold keeps the higher floor.
        assert_eq!(h.update(30, t0 + Duration::from_secs(20)), 60);
        assert_eq!(h.update(0, t0 + Duration::from_secs(31)), 60);
        // FLOOR_HOLD after the last rise, it follows the temperature again.
        assert_eq!(h.update(0, t0 + Duration::from_secs(32)), 0);
        assert_eq!(h.update(0, t0 + Duration::from_secs(40)), 0);
    }

    #[test]
    fn floor_hold_restarts_the_hold_on_a_repeat_spike() {
        let t0 = Instant::now();
        let mut h = FloorHold::default();
        h.update(45, t0);
        assert_eq!(h.update(45, t0 + Duration::from_secs(25)), 45);
        // The repeat at 25 s restarted the hold, so 40 s still holds.
        assert_eq!(h.update(0, t0 + Duration::from_secs(40)), 45);
        assert_eq!(h.update(0, t0 + Duration::from_secs(56)), 0);
    }

    #[test]
    fn floor_hold_drops_to_a_lower_floor_after_the_hold() {
        let t0 = Instant::now();
        let mut h = FloorHold::default();
        h.update(60, t0);
        assert_eq!(h.update(30, t0 + Duration::from_secs(31)), 30);
        assert_eq!(h.update(0, t0 + Duration::from_secs(50)), 30);
        assert_eq!(h.update(0, t0 + Duration::from_secs(61)), 0);
    }

    // ---- interp (fan-curve interpolation) ----

    #[test]
    fn interp_empty_is_zero() {
        assert_eq!(interp(&[], 50), 0);
    }

    #[test]
    fn interp_clamps_below_first_and_above_last() {
        let curve = [(40, 10), (80, 100)];
        // below first point -> first duty
        assert_eq!(interp(&curve, 0), 10);
        assert_eq!(interp(&curve, 40), 10);
        // above last point -> last duty
        assert_eq!(interp(&curve, 200), 100);
        assert_eq!(interp(&curve, 80), 100);
    }

    #[test]
    fn interp_exact_points_return_their_duty() {
        let curve = [(40, 0), (60, 50), (80, 100)];
        assert_eq!(interp(&curve, 40), 0);
        assert_eq!(interp(&curve, 60), 50);
        assert_eq!(interp(&curve, 80), 100);
    }

    #[test]
    fn interp_linear_midpoints() {
        let curve = [(40, 0), (80, 100)];
        assert_eq!(interp(&curve, 60), 50);
        assert_eq!(interp(&curve, 50), 25);
    }

    #[test]
    fn interp_extreme_points_do_not_overflow() {
        let curve = [(0, 0), (100, i32::MAX)];
        assert_eq!(interp(&curve, 50), 100);
        let curve = [(i32::MIN, i32::MIN), (i32::MAX, i32::MAX)];
        assert!((0..=100).contains(&interp(&curve, 50)));
        assert_eq!(interp(&[(0, -50), (100, 20)], 0), 0);
        let _ = curve_duty(&QUIET, i32::MAX, 20, i32::MAX, LONG);
    }

    #[test]
    fn interp_flat_segment_does_not_panic() {
        // Two points at the same temp: the t1 == t0 branch must not divide by zero.
        let curve = [(40, 0), (60, 20), (60, 50), (80, 100)];
        assert_eq!(interp(&curve, 60), 20);
        // surrounding interpolation still works
        assert_eq!(interp(&curve, 50), 10);
    }

    // ---- safety_floor thresholds ----

    #[test]
    fn safety_floor_thresholds() {
        assert_eq!(profiles::safety_floor(74), 0);
        assert_eq!(profiles::safety_floor(0), 0);
        assert_eq!(profiles::safety_floor(75), 30);
        assert_eq!(profiles::safety_floor(80), 45);
        assert_eq!(profiles::safety_floor(85), 60);
        assert_eq!(profiles::safety_floor(90), 80);
        assert_eq!(profiles::safety_floor(95), 80);
    }

    // ---- per-model sysfs matrix: charging profile + keyboard backlight ----
    // Charge and keyboard backlight are sysfs, not tuxedo_io. This iterates the shared
    // `tuxedoio::sim::MODELS` fixtures and, per model, builds a simulated sysfs tree, points the
    // daemon's helpers at it (TUXEDO_CHG_DIR / TUXEDO_LEDS_DIR), and asserts what our code
    // actually enforces: charging presence-gating + enum validation, and keyboard discovery +
    // per-model max_brightness. Both env vars are process-global and cargo runs tests in
    // parallel threads, so this is the SOLE test that uses them — no other test races it.
    #[test]
    fn sysfs_model_matrix() {
        use tuxedoio::sim::{KbdBacklight, MODELS};
        let root = std::env::temp_dir().join(format!("tuxedo-sysfs-matrix-{}", std::process::id()));

        for (i, m) in MODELS.iter().enumerate() {
            let base = root.join(format!("m{i}-{:#x}", m.model_id));
            let chg = base.join("charging");
            let leds = base.join("leds");
            std::fs::create_dir_all(&leds).unwrap();

            // --- charging: the attribute dir exists only if the board supports it ---
            if m.charging {
                std::fs::create_dir_all(&chg).unwrap();
                std::fs::write(chg.join("charging_profile"), "balanced").unwrap();
                std::fs::write(
                    chg.join("charging_profiles_available"),
                    "high_capacity balanced stationary\n",
                )
                .unwrap();
                std::env::set_var("TUXEDO_CHG_DIR", &chg);
            } else {
                // A path that does not exist -> chg_present() is false (daemon skips charge).
                std::env::set_var("TUXEDO_CHG_DIR", base.join("no-charging"));
            }

            assert_eq!(chg_present(), m.charging, "{}: chg_present", m.name);
            if m.charging {
                // Enum enforced (bad value rejected before any write); valid values round-trip.
                assert_eq!(
                    chg_set("garbage").unwrap_err().kind(),
                    std::io::ErrorKind::InvalidInput,
                    "{}: enum validation",
                    m.name
                );
                chg_set("stationary").unwrap();
                assert_eq!(chg_get(), "stationary", "{}: round-trip", m.name);
                chg_set("high_capacity").unwrap();
                assert_eq!(chg_get(), "high_capacity", "{}: round-trip", m.name);
                assert!(chg_avail().contains("balanced"), "{}: avail", m.name);
            }

            // --- keyboard backlight: led dir present only if the board has a backlight ---
            if m.kbd_max_brightness > 0 {
                let name = match m.kbd {
                    KbdBacklight::White => "white:kbd_backlight",
                    _ => "rgb:kbd_backlight",
                };
                let led = leds.join(name);
                std::fs::create_dir_all(&led).unwrap();
                std::fs::write(led.join("max_brightness"), m.kbd_max_brightness.to_string())
                    .unwrap();
                std::fs::write(led.join("brightness"), "0").unwrap();
            }
            std::env::set_var("TUXEDO_LEDS_DIR", &leds);

            match kbd_find() {
                Some(dir) => {
                    assert!(
                        m.kbd_max_brightness > 0,
                        "{}: found a backlight but fixture has none",
                        m.name
                    );
                    assert_eq!(
                        kbd_read(&dir, "max_brightness"),
                        m.kbd_max_brightness,
                        "{}: max_brightness",
                        m.name
                    );
                    // Brightness write/read round-trips; our code writes raw (kernel clamps).
                    kbd_write(&dir, m.kbd_max_brightness).unwrap();
                    assert_eq!(
                        kbd_read(&dir, "brightness"),
                        m.kbd_max_brightness,
                        "{}: brightness round-trip",
                        m.name
                    );
                    // Negative clamps to 0 in our code.
                    kbd_write(&dir, -3).unwrap();
                    assert_eq!(
                        kbd_read(&dir, "brightness"),
                        0,
                        "{}: negative clamp",
                        m.name
                    );
                }
                None => assert_eq!(m.kbd_max_brightness, 0, "{}: expected no backlight", m.name),
            }
        }

        std::env::remove_var("TUXEDO_CHG_DIR");
        std::env::remove_var("TUXEDO_LEDS_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- keyboard backlight sysfs (dir param: no env, no races) ----

    #[test]
    fn kbd_write_then_read_roundtrip() {
        let dir = unique_dir("kbd-roundtrip");
        let d = dir.to_str().unwrap();
        kbd_write(d, 3).unwrap();
        assert_eq!(kbd_read(d, "brightness"), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kbd_write_clamps_negative_to_zero() {
        let dir = unique_dir("kbd-clamp");
        let d = dir.to_str().unwrap();
        kbd_write(d, -5).unwrap();
        assert_eq!(kbd_read(d, "brightness"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sleep_unless_wakes_early_on_term() {
        let term = AtomicBool::new(true);
        let t = Instant::now();
        sleep_unless(&term, Duration::from_secs(30));
        assert!(t.elapsed() < Duration::from_secs(1));

        let term = AtomicBool::new(false);
        let t = Instant::now();
        sleep_unless(&term, Duration::from_millis(150));
        assert!(t.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn reinit_ec_restores_driver_fan_state_and_perf() {
        use tuxedoio::sim::{mock, MODELS};
        let fix = MODELS.iter().find(|f| f.is_validated()).copied().unwrap();
        let (dev, ec) = mock(fix);
        dev.set_perf(PerfProfile::PowerSave).unwrap();
        dev.set_fan_pct(40).unwrap();
        // Hibernation: the EC comes back with 0x0751 = 0 and its own fan control.
        ec.lock().unwrap().reg_0751 = 0;
        reinit_ec(&dev, Some(PerfProfile::PowerSave));
        let st = *ec.lock().unwrap();
        assert!(st.auto, "FANAUTO must reset the driver's fan-table state");
        assert_eq!(st.reg_0751 & 0xb0, 0xa0, "perf profile must be re-applied");
    }

    #[test]
    fn suspended_total_is_stable_while_awake() {
        let a = suspended_total();
        std::thread::sleep(Duration::from_millis(50));
        assert!(suspended_total().saturating_sub(a) < Duration::from_secs(1));
    }

    #[test]
    fn silent_socket_client_does_not_block_the_server() {
        use tuxedoio::sim::{mock, MODELS};
        let dir = unique_dir("sock-timeout");
        let path = dir.join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        // Connect and send nothing, like a hung or hostile local client.
        let _client = UnixStream::connect(&path).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let dev = Arc::new(Mutex::new(mock(MODELS[0]).0));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let store = Arc::new(Mutex::new(profiles::default_store()));
        let t = Instant::now();
        handle_client(stream, &dev, &shared, &store);
        assert!(t.elapsed() < CLIENT_TIMEOUT + Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn implausible_temps_are_rejected() {
        assert!(!plausible_temp(0));
        assert!(!plausible_temp(-1));
        assert!(!plausible_temp(120));
        assert!(plausible_temp(35));
        assert!(plausible_temp(95));
    }

    #[test]
    fn kbd_read_missing_attr_is_minus_one() {
        let dir = unique_dir("kbd-missing");
        let d = dir.to_str().unwrap();
        assert_eq!(kbd_read(d, "max_brightness"), -1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
