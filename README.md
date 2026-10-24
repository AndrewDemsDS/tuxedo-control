# Tuxedo Control

A small replacement for TUXEDO laptop hardware control (fan curves, performance profiles,
keyboard backlight and charging profiles) that runs on any Linux distribution with
systemd. It was built by reverse-engineering the `tuxedo_io` kernel ioctl interface.

It ships as a Rust daemon (`tuxedo-controld`) with a CLI prober, a TUI, a GTK4 GUI and a
KDE (Kirigami) GUI. There is an Arch package ([Arch Linux and KDE](#arch-linux-and-kde)),
a NixOS module and flake ([NixOS](#nixos)), and a plain `cargo build` plus systemd units
for everything else.

[![CI](https://github.com/AndrewDemsDS/tuxedo-control/actions/workflows/ci.yml/badge.svg)](https://github.com/AndrewDemsDS/tuxedo-control/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

> ⚠️ **Safety.** This drives a **root daemon that writes to your laptop's embedded
> controller** (fan duty, EC registers). It can set the fan to 0%. The daemon enforces a
> temperature curve and restores the EC's automatic control on exit, but you run it at
> your own risk. Only **one** fan controller may run at a time, so disable `tailord`/`tccd`
> first. Validated on one model so far (see [Supported hardware](#supported-hardware)).

## Why this exists

TUXEDO hardware control on Linux has two unsatisfying options:

1. **TUXEDO Control Center (TCC)**: the official Electron app + `tccd` daemon. It is a
   large Node/Electron install (about 450 MiB on Arch), and it is not packaged at all on
   some distributions (nixpkgs, for one).
2. **tuxedo-rs / tailord**: a Rust daemon, but its fan control **misbehaves on some
   Uniwill-based AMD models**. On the reference machine (TUXEDO **InfinityBook Pro AMD
   Gen9**, board `GXxHRXx`) tailord kept the fan loud at a cold idle: a GUI-set
   `enthusiast` performance profile stuck, and re-applying `power_save` over D-Bus did
   **not** quiet the fan at 35 °C even though the active curve was 0 % below 49 °C.
   Disabling tailord and handing the fan back to the EC was the only fix that worked.

Both `tccd` and `tailord` drive the **same kernel interface**, `/dev/tuxedo_io`
(`tuxedo_io.ko`, from the GPL `tuxedo-drivers`). The plan: understand that ioctl protocol
directly, build a small correct daemon, and package it for the distributions in use. No
Electron, no guessing.

The project started as a NixOS module (it was called `tuxedo-control-nix`), so the
declarative NixOS module is still the most complete configuration surface.

## Status: all phases working on the reference machine ✅

Reference machine: InfinityBook Pro AMD Gen9 (Uniwill, `tuxedo_io` v0.3.9).

| Phase | Deliverable | State |
|---|---|---|
| 0 | Recon: [`docs/hardware-interface.md`](docs/hardware-interface.md) | ✅ |
| 1 | **Protocol map + root-cause** in [`docs/phase1-protocol.md`](docs/phase1-protocol.md): every Uniwill ioctl, fan scaling (0–0xc8), the `0x40`-bit bug | ✅ source-derived |
| 2 | **Prober** (`prober/`): drives fan 0→60→auto; confirmed the protocol + fix on hardware | ✅ validated |
| 3 | **Daemon** (`daemon/`): temp→duty curve, hysteresis, clears `0x40` each tick, perf profile decoupled, restores EC auto on `SIGTERM` | ✅ validated |
| 4 | **NixOS module + flake** (`nix/`): `services.tuxedo-control`, declarative curve/profile; force-disables `tuxedo-rs`, loads `tuxedo_io` | ✅ evals + builds |
| 5 | **TUI** (`tui/`): live temps/fan/mode dashboard + quick controls | ✅ |
| 6 | **GUI** (`gui/`): libadwaita/GTK4 app — fan, performance profiles, keyboard backlight, charging profile, follow-system theme | ✅ |

Beyond the phases: **named performance profiles** with their own fan curves
(create/delete/set-default), built-in profiles modelled on TUXEDO Control Center, and
**import of TCC's exported profiles**. CI (`nix flake check` + clippy/fmt) runs on every
push, and the first tagged release is **[v0.1.0](../../releases/tag/v0.1.0)** (see
[CHANGELOG.md](CHANGELOG.md)).

**The bug, in one line:** `uw_set_fan` ignores the requested duty unless bit `0x40`
of EC RAM `0x0751` is clear. Once anything (a stuck perf/mode write) sets `0x40`, the EC
keeps the fan on its own loud curve. tuxedo-rs/tailord doesn't guard against this; this
daemon clears `0x40` before every fan write. Full analysis in `docs/phase1-protocol.md`.

## Usage

```sh
cargo build --release            # workspace: prober, daemon, tui   (or: nix build .#default)

sudo ./target/release/tuxedo-prober info     # read temps/fan/mode
sudo ./target/release/tuxedo-prober set 40   # both fans -> 40%
sudo ./target/release/tuxedo-prober auto     # hand back to the EC
sudo ./target/release/tuxedo-tui             # live dashboard
```

## NixOS

Add this flake as an input and enable the module:

```nix
services.tuxedo-control = {
  enable = true;
  performanceProfile = "power_save";
  fan.curve = [
    { temp = 25; speed = 0; } { temp = 50; speed = 0; }
    { temp = 62; speed = 24; } { temp = 80; speed = 60; } { temp = 90; speed = 100; }
  ];
};
```

> The driver enforces a ~25 % on-speed floor; a requested duty below ~12 % becomes 0 (off).

## Arch Linux and KDE

[`dist/arch/PKGBUILD`](dist/arch/PKGBUILD) builds two packages:

- `tuxedo-control-git`: `tuxedo-controld`, the TUI, the prober, a systemd unit and a
  `modules-load.d` entry for `tuxedo_io`.
- `tuxedo-control-kde-git`: a Kirigami (Qt 6) GUI in [`kde/`](kde) that replaces the
  GTK4 app on Plasma. It covers the same controls (live status, named profiles with a
  drag-to-edit fan curve, EC performance mode, manual fan, keyboard backlight, charging,
  TCC import) plus a selector for the TLP power profile.

```sh
cd dist/arch && makepkg -si
sudo systemctl disable --now tccd tccd-sleep     # one fan controller at a time
sudo systemctl enable --now tuxedo-control
sudo systemctl enable tuxedo-control-sleep       # re-initialise the EC after resume
```

Other distributions: `cargo build --release`, install the binaries to `/usr/bin`, and copy
the units from [`dist/systemd/`](dist/systemd) (plus `tuxedo-control.conf` to
`/etc/modules-load.d/`).

`tuxedo-control-sleep.service` stops the daemon before sleep and starts it on resume:
hibernation resets the EC, and the daemon's start-up sets it up again. The main unit declares `Conflicts=tccd.service tailord.service`. Stopping it sends SIGTERM,
and the daemon hands the fan back to the EC before it exits.

### With TLP

[`tuxedo-tlp`](https://github.com/AndrewDemsDS/tuxedo-tlp) splits the work: TLP owns CPU
policy and the charge ceiling, `tuxedo-controld` owns fans, EC mode and keyboard. Its
`tuxedo-tlp-controld-sync` service follows `tlp-pd`'s active profile and activates a
matching controld profile (power-saver to Max Energy Save, balanced to Quiet,
performance to High Performance). Keep `charge` unset in controld profiles and leave
`profile` out of `/etc/tuxedo-control/config.json`, or the daemon re-applies it on every
restart and overrides the sync.

## Roadmap (next)

The initial roadmap (phases 0–6, declarative module options, a NixOS VM test, and model
gating via `R_UW_MODEL_ID`) is complete. Model gating now refuses fan/EC writes on any board
not in the known-models registry, so the daemon is safe to run on other Uniwill boards
(read-only until validated). Future direction is breadth: validating and adding more boards
to the registry — see [docs/model-gating.md](docs/model-gating.md) and
[CONTRIBUTING](CONTRIBUTING.md#adding-support-for-a-new-board).

## Methodology (source-first, no black-box guessing)

- **The kernel module is GPL.** `tuxedo-drivers` defines the ioctl request numbers and
  structs. Read them; don't reverse the wire blind.
- **`strace -e ioctl` the existing daemons** (`tailord`, and `tccd` if obtainable) to see
  the exact `TUXEDO_IO_*` calls and argument values on *this* board.
- **Cross-reference the two implementations**: tuxedo-rs (Rust, MIT) and TCC (`tccd`, GPL).
  The divergence explains the Uniwill-AMD fan bug.
- Validate every step on real hardware via the prober before baking it into the daemon.

## Prior art / references

- `tuxedo-drivers`: the GPL kernel modules (ioctl source of truth).
- `tuxedo-rs` (tailord): Rust daemon, MIT; the base to fix or learn from.
- `tuxedo-control-center`: the official Electron app + `tccd`.
- `blitz/tuxedo-nixos`: an existing community flake; evaluate as a packaging reference.

(Exact upstream URLs collected in [`docs/reverse-engineering-plan.md`](docs/reverse-engineering-plan.md).)

## Supported hardware

A board is listed as **validated** only once fan control has been exercised on it.

| Model | Board | `tuxedo_io` | Status |
|---|---|---|---|
| TUXEDO InfinityBook Pro AMD Gen9 | `GXxHRXx` (Uniwill) | 0.3.9 | ✅ validated: fan, perf profile, keyboard backlight, charging profile |

Other Uniwill TUXEDO laptops may work but are **unverified**. Adding a board is a
deliberate, source-first process. See [CONTRIBUTING](CONTRIBUTING.md#adding-support-for-a-new-board).
Features that the EC doesn't expose (e.g. TDP control returns `ENODEV` on the reference
board) are detected at runtime and hidden rather than guessed.

## Versioning

Pre-1.0. While `0.y.z`, any `0.y` release may change the CLI, the daemon's socket
protocol, the NixOS module options, or ioctl behaviour. See [CHANGELOG.md](CHANGELOG.md).

## Contributing

Issues and PRs welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) (especially the
hardware-safety section) and the [Code of Conduct](CODE_OF_CONDUCT.md). For security or
physical-safety issues, follow [SECURITY.md](SECURITY.md); do not open a public issue.

## AI assistance

Parts of this project — code, Nix packaging, and docs — were written with AI assistance.
I reviewed, tested, and validated everything that shipped, especially the root daemon that
writes to the embedded controller, before committing it.

## License

[MIT](LICENSE).
