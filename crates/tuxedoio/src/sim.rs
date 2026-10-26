//! In-memory simulation of the `/dev/tuxedo_io` EC for tests (no hardware required).
//!
//! [`MockEc`] reproduces the ioctl semantics our control logic depends on — model id,
//! capability probes, fan-duty scaling, the `0x40` fan-ownership behaviour (the bug the daemon
//! works around), and the performance-profile register bits — keyed by a [`ModelFixture`].
//!
//! Model ids and the universal-vs-legacy fan max are taken from the GPL `tuxedo-drivers`
//! (see docs/model-gating.md). Per-model register values the source does not pin down are
//! marked "imaginary" and invented for coverage. Build a simulated device with [`mock`] and
//! inspect what it received through the returned [`EcHandle`].

use std::io;
use std::sync::{Arc, Mutex};

use crate::{
    known_model, EcBackend, TuxedoIo, FAN_OWNERSHIP_BIT, R_HWCHECK_UW, R_UW_FANSPEED,
    R_UW_FANSPEED2, R_UW_FANS_MIN_SPEED, R_UW_FANS_OFF_AVAILABLE, R_UW_FAN_TEMP, R_UW_FAN_TEMP2,
    R_UW_MODE, R_UW_MODEL_ID, R_UW_PROFS_AVAILABLE, W_UW_FANAUTO, W_UW_FANSPEED, W_UW_FANSPEED2,
    W_UW_MODE, W_UW_PERF_PROF,
};

/// Keyboard backlight class (informational, for the per-model matrix; not on the tuxedo_io path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KbdBacklight {
    None,
    White,
    OneZoneRgb,
    PerKeyRgb,
}

/// A simulated board. `model_id`, `is_uniwill`, `fan_max`, `profs_available`, `fans_off`,
/// `fans_min_speed` drive the EC ioctls; `charging`/`kbd` are metadata for the sysfs-side
/// matrix. Whether writes are allowed is NOT stored here — it is derived from
/// [`crate::known_model`] so the fixture can never disagree with the real registry.
#[derive(Clone, Copy, Debug)]
pub struct ModelFixture {
    pub model_id: i32,
    pub name: &'static str,
    pub is_uniwill: bool,
    pub fan_max: i32,
    pub fans_off: bool,
    pub fans_min_speed: i32,
    pub profs_available: i32,
    pub charging: bool,
    pub kbd: KbdBacklight,
    /// `max_brightness` the keyboard-backlight LED exposes (0 = no backlight). Real driver
    /// values: white-only `2`, 5-level white / 1-zone RGB `4`, Clevo `255` (per `*_leds.h`).
    pub kbd_max_brightness: i32,
}

impl ModelFixture {
    /// True when this model is in the real [`crate::KNOWN_MODELS`] registry, i.e. the daemon
    /// will allow EC writes on it. Tests assert `dev.write_allowed() == fixture.is_validated()`.
    pub fn is_validated(&self) -> bool {
        known_model(self.model_id).is_some()
    }
}

/// Representative fixtures. The reference board (`0x1a`) is the only validated entry — every
/// other model must be refused EC writes (read-only). Real ids: `0x09/0x12/0x13/0x14/0x17`
/// from `tuxedo-drivers` (`uniwill_interfaces.h`); `0x2a`/`0x2b` are imaginary "unknown
/// Uniwill" boards; the Clevo entry has no readable Uniwill model id (`is_uniwill = false`).
pub const MODELS: &[ModelFixture] = &[
    ModelFixture {
        model_id: 0x1a,
        name: "InfinityBook Pro AMD Gen9 (GXxHRXx) [validated]",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 3,
        charging: true,
        kbd: KbdBacklight::White,
        kbd_max_brightness: 4, // 5-level white (0..4, matches the GUI scale)
    },
    ModelFixture {
        model_id: 0x09,
        name: "PF5LUXG Pulse Gen2",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 2,
        charging: false, // PF5xx is excluded from charging-profile support
        kbd: KbdBacklight::OneZoneRgb,
        kbd_max_brightness: 4,
    },
    ModelFixture {
        model_id: 0x12,
        name: "PH4TRX IBP Gen6 (legacy-path exception)",
        is_uniwill: true,
        fan_max: 0xff, // legacy direct path: raw max 255 (imaginary for this id, per source)
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 3,
        charging: true,
        kbd: KbdBacklight::White,
        kbd_max_brightness: 2,
    },
    ModelFixture {
        model_id: 0x13,
        name: "PH4TUX IBP Gen6",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 3,
        charging: true,
        kbd: KbdBacklight::White,
        kbd_max_brightness: 2,
    },
    ModelFixture {
        model_id: 0x14,
        name: "PH4TQF IBP Gen6",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 3,
        charging: true,
        kbd: KbdBacklight::White,
        kbd_max_brightness: 2,
    },
    ModelFixture {
        model_id: 0x17,
        name: "PH4AQF/ARX IBP Gen7",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 3,
        charging: true,
        kbd: KbdBacklight::PerKeyRgb,
        kbd_max_brightness: 4,
    },
    ModelFixture {
        model_id: 0x2a,
        name: "Imaginary future Uniwill (2 profiles, no charging)",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: false,
        fans_min_speed: 30,
        profs_available: 2,
        charging: false,
        kbd: KbdBacklight::OneZoneRgb,
        kbd_max_brightness: 4,
    },
    ModelFixture {
        model_id: 0x2b,
        name: "Imaginary future Uniwill (no perf profiles)",
        is_uniwill: true,
        fan_max: 0xc8,
        fans_off: true,
        fans_min_speed: 25,
        profs_available: 0,
        charging: true,
        kbd: KbdBacklight::None,
        kbd_max_brightness: 0, // no keyboard backlight
    },
    ModelFixture {
        model_id: 0x00,
        name: "Imaginary Clevo (not Uniwill)",
        is_uniwill: false,
        fan_max: 0xff,
        fans_off: true,
        fans_min_speed: 20,
        profs_available: 0,
        charging: false,
        kbd: KbdBacklight::PerKeyRgb,
        kbd_max_brightness: 255, // Clevo CLEVO_KBD_BRIGHTNESS_MAX = 0xff
    },
];

/// Mutable simulated EC registers. Tests read/write this through the [`EcHandle`].
#[derive(Clone, Copy, Debug)]
pub struct EcState {
    /// EC RAM 0x0751: fan-ownership bit `0x40` + performance-profile bits (`0xa0`/`0x10`).
    pub reg_0751: i32,
    pub fan1_raw: i32,
    pub fan2_raw: i32,
    pub auto: bool,
    pub cpu_temp: i32,
    pub gpu_temp: i32,
    /// Last performance-profile id written (0 = none).
    pub last_perf: i32,
    /// Model-id reads that return 0 because the driver's feature probe hit an EC error. The
    /// driver caches 0 and re-probes on the next `R_HWCHECK_UW`, so each identify uses one up.
    pub model_probe_failures: u32,
}

impl Default for EcState {
    fn default() -> Self {
        EcState {
            reg_0751: 0,
            fan1_raw: 0,
            fan2_raw: 0,
            auto: false,
            cpu_temp: 45,
            gpu_temp: 0,
            last_perf: 0,
            model_probe_failures: 0,
        }
    }
}

/// Shared, inspectable handle to a simulated EC's [`EcState`].
pub type EcHandle = Arc<Mutex<EcState>>;

/// Simulated EC backend for one [`ModelFixture`].
pub struct MockEc {
    fix: ModelFixture,
    st: EcHandle,
}

impl MockEc {
    pub fn new(fix: ModelFixture) -> (Self, EcHandle) {
        let st: EcHandle = Arc::new(Mutex::new(EcState::default()));
        (
            Self {
                fix,
                st: st.clone(),
            },
            st,
        )
    }
}

impl EcBackend for MockEc {
    fn rd(&self, req: libc::c_ulong) -> io::Result<i32> {
        let mut st = self.st.lock().unwrap();
        let v = match req {
            r if r == R_HWCHECK_UW => {
                st.model_probe_failures = st.model_probe_failures.saturating_sub(1);
                self.fix.is_uniwill as i32
            }
            r if r == R_UW_MODEL_ID && st.model_probe_failures > 0 => 0,
            r if r == R_UW_MODEL_ID => self.fix.model_id,
            r if r == R_UW_FAN_TEMP => st.cpu_temp,
            r if r == R_UW_FAN_TEMP2 => st.gpu_temp,
            r if r == R_UW_FANSPEED => st.fan1_raw,
            r if r == R_UW_FANSPEED2 => st.fan2_raw,
            r if r == R_UW_MODE => st.reg_0751,
            r if r == R_UW_FANS_OFF_AVAILABLE => self.fix.fans_off as i32,
            r if r == R_UW_FANS_MIN_SPEED => self.fix.fans_min_speed,
            r if r == R_UW_PROFS_AVAILABLE => self.fix.profs_available,
            _ => 0, // unknown read: benign zero (matches a quiet EC)
        };
        Ok(v)
    }

    fn wr(&self, req: libc::c_ulong, v: i32) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        match req {
            r if r == W_UW_FANSPEED || r == W_UW_FANSPEED2 => {
                // Reproduce the EC bug: a fan write is a no-op unless the 0x40 ownership bit
                // is clear. The daemon clears it via ensure_manual() before every fan write.
                if st.reg_0751 & FAN_OWNERSHIP_BIT == 0 {
                    let raw = v.clamp(0, self.fix.fan_max);
                    if req == W_UW_FANSPEED {
                        st.fan1_raw = raw;
                    } else {
                        st.fan2_raw = raw;
                    }
                }
                Ok(())
            }
            r if r == W_UW_MODE => {
                st.reg_0751 = v;
                Ok(())
            }
            r if r == W_UW_PERF_PROF => {
                // Simulated profile enforcement: a board rejects a profile it doesn't expose
                // (0 profiles => none; 2 profiles => no overboost(3)).
                let unsupported =
                    self.fix.profs_available == 0 || (self.fix.profs_available == 2 && v == 3);
                if unsupported {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "performance profile not available on this model",
                    ));
                }
                // Clear both profile bits, then set per id (1=powersave 0xa0, 2=enthusiast 0x00,
                // 3=overboost 0x10) — matches the driver's 0x0751 layout.
                st.reg_0751 &= !(0xa0 | 0x10);
                st.reg_0751 |= match v {
                    1 => 0xa0,
                    3 => 0x10,
                    _ => 0x00,
                };
                st.last_perf = v;
                Ok(())
            }
            _ => Ok(()), // unknown write: accept and ignore
        }
    }

    fn noarg(&self, req: libc::c_ulong) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        if req == W_UW_FANAUTO {
            st.auto = true;
        }
        Ok(())
    }

    fn version(&self) -> io::Result<String> {
        Ok("0.3.9-sim".to_string())
    }
}

/// Build a gated [`TuxedoIo`] over a simulated board (writes allowed only if the model is in
/// [`crate::KNOWN_MODELS`]), returning the device plus a handle to inspect the simulated EC.
pub fn mock(fix: ModelFixture) -> (TuxedoIo, EcHandle) {
    let (be, h) = MockEc::new(fix);
    (TuxedoIo::from_backend(Box::new(be)), h)
}

/// Like [`mock`] but starting from a given EC state (applied before the model gate runs).
pub fn mock_with(fix: ModelFixture, state: EcState) -> (TuxedoIo, EcHandle) {
    let (be, h) = MockEc::new(fix);
    *h.lock().unwrap() = state;
    (TuxedoIo::from_backend(Box::new(be)), h)
}

/// Like [`mock`] but bypassing the model write-gate (as the prober does on real hardware).
pub fn mock_unchecked(fix: ModelFixture) -> (TuxedoIo, EcHandle) {
    let (be, h) = MockEc::new(fix);
    (TuxedoIo::from_backend_unchecked(Box::new(be)), h)
}
