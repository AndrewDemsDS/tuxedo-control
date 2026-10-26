//! Minimal wrappers over the TUXEDO `/dev/tuxedo_io` Uniwill ioctl interface.
//!
//! Protocol + scaling derived from `tuxedo-drivers` and validated on an InfinityBook Pro
//! AMD Gen9 (Uniwill "NB02" path). See `docs/phase1-protocol.md`.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::io::AsRawFd;

// ---- Linux _IOC encoding ----
const NRBITS: u32 = 8;
const TYPEBITS: u32 = 8;
const SIZEBITS: u32 = 14;
const TYPESHIFT: u32 = NRBITS;
const SIZESHIFT: u32 = TYPESHIFT + TYPEBITS;
const DIRSHIFT: u32 = SIZESHIFT + SIZEBITS;
const DIR_NONE: u32 = 0;
const DIR_WRITE: u32 = 1;
const DIR_READ: u32 = 2;

const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << DIRSHIFT) | (ty << TYPESHIFT) | nr | (size << SIZESHIFT)) as libc::c_ulong
}
// IMPORTANT: the driver header types the arg as `int32_t*` / `char*` (a POINTER), so the
// encoded size is sizeof(pointer)=8 on x86_64, not sizeof(int32_t). (Empirically required.)
const PTR: u32 = 8;
const fn ior(ty: u32, nr: u32) -> libc::c_ulong {
    ioc(DIR_READ, ty, nr, PTR)
}
const fn iow(ty: u32, nr: u32) -> libc::c_ulong {
    ioc(DIR_WRITE, ty, nr, PTR)
}

const MAGIC: u32 = 0xEC;
const RD_UW: u32 = MAGIC + 3;
const WR_UW: u32 = MAGIC + 4;

pub const R_MOD_VERSION: libc::c_ulong = ior(MAGIC, 0x00);
pub const R_HWCHECK_UW: libc::c_ulong = ior(MAGIC, 0x06);
pub const R_UW_MODEL_ID: libc::c_ulong = ior(RD_UW, 0x01);
pub const R_UW_FANSPEED: libc::c_ulong = ior(RD_UW, 0x10);
pub const R_UW_FANSPEED2: libc::c_ulong = ior(RD_UW, 0x11);
pub const R_UW_FAN_TEMP: libc::c_ulong = ior(RD_UW, 0x12);
pub const R_UW_FAN_TEMP2: libc::c_ulong = ior(RD_UW, 0x13);
pub const R_UW_MODE: libc::c_ulong = ior(RD_UW, 0x14); // EC RAM 0x0751
pub const R_UW_FANS_OFF_AVAILABLE: libc::c_ulong = ior(RD_UW, 0x16);
pub const R_UW_FANS_MIN_SPEED: libc::c_ulong = ior(RD_UW, 0x17);
pub const R_UW_PROFS_AVAILABLE: libc::c_ulong = ior(RD_UW, 0x21);
pub const W_UW_FANSPEED: libc::c_ulong = iow(WR_UW, 0x10);
pub const W_UW_FANSPEED2: libc::c_ulong = iow(WR_UW, 0x11);
pub const W_UW_MODE: libc::c_ulong = iow(WR_UW, 0x12); // EC RAM 0x0751
pub const W_UW_FANAUTO: libc::c_ulong = ioc(DIR_NONE, WR_UW, 0x14, 0);
pub const W_UW_PERF_PROF: libc::c_ulong = iow(WR_UW, 0x18);

/// Fan duty 100% on the NB02 path = 0xc8 (200).
pub const FAN_MAX: i32 = 0xc8;
/// Bit in EC RAM 0x0751: when SET, `uw_set_fan` is a no-op and the EC owns the fan.
pub const FAN_OWNERSHIP_BIT: i32 = 0x40;

/// A board validated on real hardware: its model id and fan-duty raw max. Only boards that
/// have been exercised with the prober belong here; the daemon refuses fan/EC writes on any
/// model id not listed (read-only mode). See docs/model-gating.md.
pub struct KnownModel {
    pub model_id: i32,
    pub name: &'static str,
    pub fan_max: i32,
}

/// Registry of validated boards. `fan_max` is tied to `FAN_MAX` because the scaling helpers
/// (`pct_to_raw`/`raw_to_pct`) use that constant; a board with a different raw max must not
/// be added here until the scaling is made per-model.
pub const KNOWN_MODELS: &[KnownModel] = &[KnownModel {
    model_id: 0x1a,
    name: "InfinityBook Pro AMD Gen9 (GXxHRXx)",
    fan_max: FAN_MAX,
}];

/// Look up a validated board by model id; `None` means unsupported (run read-only).
pub fn known_model(model_id: i32) -> Option<&'static KnownModel> {
    KNOWN_MODELS.iter().find(|m| m.model_id == model_id)
}

/// Model id + capability probes read from the EC (see docs/model-gating.md).
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub model_id: i32,
    pub fans_off: bool,
    pub fans_min_speed: i32,
    pub profs_available: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfProfile {
    PowerSave = 1,
    Enthusiast = 2,
    Overboost = 3,
}

impl PerfProfile {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "power_save" | "powersave" | "power-save" => Some(Self::PowerSave),
            "enthusiast" | "balanced" => Some(Self::Enthusiast),
            "overboost" | "performance" => Some(Self::Overboost),
            _ => None,
        }
    }
    /// Canonical id used in config + the GUI/socket protocol (stable, underscored).
    pub fn as_id(&self) -> &'static str {
        match self {
            Self::PowerSave => "power_save",
            Self::Enthusiast => "enthusiast",
            Self::Overboost => "overboost",
        }
    }
}

pub fn pct_to_raw(p: i32) -> i32 {
    (p.clamp(0, 100) * FAN_MAX / 100).clamp(0, FAN_MAX)
}
pub fn raw_to_pct(r: i32) -> i32 {
    (r * 100 + FAN_MAX / 2) / FAN_MAX
}

/// Raw ioctl backend for a [`TuxedoIo`]. The real implementation talks to `/dev/tuxedo_io`;
/// tests substitute a simulated EC (see [`sim`]). The write-gate is intentionally NOT here —
/// it lives in `TuxedoIo::wr`/`noarg`, so the safety property holds for any backend.
pub trait EcBackend: Send {
    fn rd(&self, req: libc::c_ulong) -> io::Result<i32>;
    fn wr(&self, req: libc::c_ulong, v: i32) -> io::Result<()>;
    fn noarg(&self, req: libc::c_ulong) -> io::Result<()>;
    fn version(&self) -> io::Result<String>;
}

/// The real backend: ioctls on an open `/dev/tuxedo_io` handle.
struct DeviceBackend {
    file: std::fs::File,
}
impl EcBackend for DeviceBackend {
    fn rd(&self, req: libc::c_ulong) -> io::Result<i32> {
        let mut v: i32 = 0;
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), req, &mut v as *mut i32) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(v)
    }
    fn wr(&self, req: libc::c_ulong, mut v: i32) -> io::Result<()> {
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), req, &mut v as *mut i32) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn noarg(&self, req: libc::c_ulong) -> io::Result<()> {
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), req) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn version(&self) -> io::Result<String> {
        let mut buf = [0u8; 64];
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), R_MOD_VERSION, buf.as_mut_ptr()) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}

/// Open `/dev/tuxedo_io` and run the Uniwill identify ioctl before anything else.
///
/// tuxedo_io caches the Uniwill feature table (`uw_feats`) only when `uniwill_identify()`
/// succeeds, at module init or on `R_HWCHECK_UW`. If the module loads before uniwill_wmi has
/// registered its interface (modules-load.d runs early), init leaves `uw_feats` NULL and every
/// other `R_UW_*` read that touches it (e.g. `R_UW_MODEL_ID`) is a kernel NULL dereference that
/// oopses with IRQs off. `R_HWCHECK_UW` re-runs identify, so it must be the first Uniwill call;
/// a negative answer is an error here, never a reason to carry on.
fn open_device() -> io::Result<DeviceBackend> {
    let dev = DeviceBackend {
        file: OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tuxedo_io")?,
    };
    if dev.rd(R_HWCHECK_UW)? != 1 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "tuxedo_io: Uniwill interface not identified (uniwill_wmi not ready or not a Uniwill board)",
        ));
    }
    Ok(dev)
}

pub struct TuxedoIo {
    backend: Box<dyn EcBackend>,
    /// Writes (fan duty, perf, mode, fan-auto) are refused unless the board is validated.
    /// This is the safety gate: every EC-writing consumer (daemon, TUI) inherits it.
    write_allowed: bool,
}

impl TuxedoIo {
    /// Open with the model gate: reads are always allowed, but writes are refused on a board
    /// whose model id is not in [`KNOWN_MODELS`] (an unvalidated board's fan scaling/ownership
    /// behaviour is unknown). Use [`TuxedoIo::open_unchecked`] for the prober, which must be
    /// able to write in order to validate a new board.
    pub fn open() -> io::Result<Self> {
        Ok(Self::from_backend(Box::new(open_device()?)))
    }

    /// Open WITHOUT the model write-gate. Reserved for the prober's source-first validation
    /// of a new board (reads first, then writes with `auto` as the bail-out).
    pub fn open_unchecked() -> io::Result<Self> {
        Ok(Self::from_backend_unchecked(Box::new(open_device()?)))
    }

    /// Wrap any backend, applying the model write-gate: reads the model id and allows writes
    /// only if it is in [`KNOWN_MODELS`], else read-only. `pub(crate)` so only the in-crate
    /// [`sim`] harness can substitute a backend; external callers use [`TuxedoIo::open`].
    pub(crate) fn from_backend(backend: Box<dyn EcBackend>) -> Self {
        let mut me = Self {
            backend,
            write_allowed: false,
        };
        // Best-effort: if the model id can't be read, stay locked (fail safe to read-only).
        me.write_allowed = me
            .model_id()
            .map(|id| known_model(id).is_some())
            .unwrap_or(false);
        me
    }

    /// Wrap any backend WITHOUT the model write-gate. `pub(crate)`: keeps the ungated
    /// constructor off the public API (the real ungated path is [`TuxedoIo::open_unchecked`]).
    pub(crate) fn from_backend_unchecked(backend: Box<dyn EcBackend>) -> Self {
        Self {
            backend,
            write_allowed: true,
        }
    }

    /// Whether EC writes are permitted (i.e. the board is in [`KNOWN_MODELS`]).
    pub fn write_allowed(&self) -> bool {
        self.write_allowed
    }

    pub fn rd(&self, req: libc::c_ulong) -> io::Result<i32> {
        self.backend.rd(req)
    }
    pub fn wr(&self, req: libc::c_ulong, v: i32) -> io::Result<()> {
        if !self.write_allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "EC writes disabled: unvalidated board model",
            ));
        }
        self.backend.wr(req, v)
    }
    pub fn noarg(&self, req: libc::c_ulong) -> io::Result<()> {
        if !self.write_allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "EC writes disabled: unvalidated board model",
            ));
        }
        self.backend.noarg(req)
    }

    pub fn is_uniwill(&self) -> io::Result<bool> {
        Ok(self.rd(R_HWCHECK_UW)? == 1)
    }
    pub fn cpu_temp(&self) -> io::Result<i32> {
        self.rd(R_UW_FAN_TEMP)
    }
    pub fn gpu_temp(&self) -> io::Result<i32> {
        self.rd(R_UW_FAN_TEMP2)
    }
    pub fn cpu_fan_pct(&self) -> io::Result<i32> {
        Ok(raw_to_pct(self.rd(R_UW_FANSPEED)?))
    }
    pub fn gpu_fan_pct(&self) -> io::Result<i32> {
        Ok(raw_to_pct(self.rd(R_UW_FANSPEED2)?))
    }
    pub fn mode(&self) -> io::Result<i32> {
        self.rd(R_UW_MODE)
    }

    /// Board model id (EC RAM 0x0740 via R_UW_MODEL_ID). Used to gate behaviour to
    /// validated boards; see docs/model-gating.md.
    pub fn model_id(&self) -> io::Result<i32> {
        self.rd(R_UW_MODEL_ID)
    }
    /// Whether the EC allows the fan to stop (0%). Uniform `true` on the current driver.
    pub fn fans_off_available(&self) -> io::Result<bool> {
        Ok(self.rd(R_UW_FANS_OFF_AVAILABLE)? != 0)
    }
    /// On-speed floor in percent reported by the EC (driver default 25). The control loop
    /// can honour this instead of a hardcoded floor.
    pub fn fans_min_speed(&self) -> io::Result<i32> {
        self.rd(R_UW_FANS_MIN_SPEED)
    }
    /// Number of performance profiles the board exposes (0, 2, or 3); model-gated.
    pub fn profs_available(&self) -> io::Result<i32> {
        self.rd(R_UW_PROFS_AVAILABLE)
    }
    /// Read model id + capability probes together.
    pub fn caps(&self) -> io::Result<Caps> {
        Ok(Caps {
            model_id: self.model_id()?,
            fans_off: self.fans_off_available()?,
            fans_min_speed: self.fans_min_speed()?,
            profs_available: self.profs_available()?,
        })
    }

    /// Clear bit 0x40 of 0x0751 so manual fan writes are honoured. Returns true if it
    /// had to fix it (i.e. the EC had grabbed fan ownership).
    pub fn ensure_manual(&self) -> io::Result<bool> {
        let m = self.mode()?;
        if m & FAN_OWNERSHIP_BIT != 0 {
            self.wr(W_UW_MODE, m & !FAN_OWNERSHIP_BIT)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Set both fans to a duty percent, clearing the ownership bit first (the bug fix).
    pub fn set_fan_pct(&self, pct: i32) -> io::Result<()> {
        self.ensure_manual()?;
        let raw = pct_to_raw(pct);
        self.wr(W_UW_FANSPEED, raw)?;
        self.wr(W_UW_FANSPEED2, raw)?;
        Ok(())
    }

    pub fn set_perf(&self, p: PerfProfile) -> io::Result<()> {
        self.wr(W_UW_PERF_PROF, p as i32)
    }
    pub fn restore_auto(&self) -> io::Result<()> {
        self.noarg(W_UW_FANAUTO)
    }

    pub fn version(&self) -> io::Result<String> {
        self.backend.version()
    }
}

pub mod sim;
