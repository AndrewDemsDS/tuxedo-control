# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While pre-1.0 (`0.y.z`), the public API, the NixOS module options, and ioctl behaviour
may change in any `0.y` release. See "Versioning" in the README.

## [Unreleased]

### Fixed
- Kernel NULL dereference in tuxedo_io on every daemon start when `tuxedo_io` loaded before
  `uniwill_wmi` (the `modules-load.d` entry makes that the normal case). The library read
  `R_UW_MODEL_ID` before `R_HWCHECK_UW`, so tuxedo_io's feature table was still NULL. Every
  open now runs the identify ioctl first and fails cleanly if the interface is not ready; the
  unit's restart retries until it is.
- The daemon unit (Arch and NixOS) restarted forever when the daemon could not start. With
  `RestartSec=3` systemd's default start limit (5 starts in 10 s) never trips, so a failing
  start looped every 3 s all night. The unit now allows 10 starts in 120 s, enough to wait for
  `uniwill_wmi` at boot, and then stays failed.
- A stop (including the one before suspend) could SIGKILL the daemon before it handed the fan
  back to the EC. The loop slept a full poll interval before noticing SIGTERM, and the first
  fan write after start (EC fan-table init, ~13 s) outlasted `TimeoutStopSec=10`. The loop now
  wakes on SIGTERM, skips the fan write once asked to stop, and the unit allows 30 s (the
  NixOS unit had no limit and used systemd's 90 s). `poll_seconds = 0` no longer busy-loops.
- A failed EC read of the mode register (0x0751) could be written back as garbage. tuxedo_io
  never fails a Uniwill read: an EC error comes back as `0xfe`, and clearing the fan-ownership
  bit from that wrote `0xbe` (both performance bits plus undefined bits). `mode()` now rejects
  values with bits outside the documented set, so `set_fan_pct` errors instead of writing, and
  out-of-range fan duty reads are rejected instead of reported as 127 %. The prober still
  prints the raw register.
- One failed EC read during the driver's model probe left a validated board read-only until
  the next restart. The driver caches model id 0 on that error and re-probes on the next
  identify; the write gate now re-runs `R_HWCHECK_UW` (up to 3 times) before accepting 0.
- After hibernation the daemon's fan writes could be silently ignored. tuxedo_io programs the
  EC's custom fan table only on the first write after load or after `FANAUTO`, and that state
  survives sleep while the EC is reset. The daemon now issues `FANAUTO` at startup (covers a
  previously killed instance) and, when it detects a resume itself (CLOCK_BOOTTIME jumping
  ahead of CLOCK_MONOTONIC), re-runs that setup and re-applies the perf profile. This no
  longer depends on `tuxedo-control-sleep.service` being enabled.
- One socket client that connected and never sent a full line blocked the daemon's control
  socket for every other client (the GUI froze) because requests are served one at a time with
  no read timeout. Clients now time out after 2 s and a request line is capped at 64 KiB.
- A power loss or failed resume while the profile store was being written could leave
  `profiles.json` empty or truncated, and the next start silently reset to the built-in
  profiles. The store is now written to a synced temp file and renamed into place.
- Fan-curve interpolation overflowed `i32` on curves with large values (accepted unchecked
  from `SAVEPROFILE` and TCC imports): a panic in debug builds, and in release a wrapped,
  possibly negative duty that switched the fan off. The math is now done in `i128` and the
  duty is clamped to 0..=100.
- Quitting `tuxedo-tui` after a `+`/`-` fan nudge left the fan pinned at that duty (even 0 %)
  with nothing driving it, since the TUI runs with the daemon stopped. It now restores EC
  automatic control on exit when it set a manual duty.

## [0.3.0] - 2026-10-24

### Changed
- Renamed the project from `tuxedo-control-nix` to Tuxedo Control (`tuxedo-control`). It runs
  on any systemd distribution; the NixOS module stays. The old GitHub URL redirects.

### Added
- Arch Linux packaging (`dist/arch/PKGBUILD`): `tuxedo-control-git` (daemon, TUI, prober,
  systemd units, `modules-load.d` entry) and `tuxedo-control-kde-git`, a Kirigami (Qt 6) GUI
  for Plasma with live status, named profiles with a drag-to-edit fan curve, EC performance
  mode, manual fan, keyboard backlight, charging, TCC import and a TLP power-profile selector.
- `STATUS` carries `ready`, false until the loop's first EC reading (about 13 s after start on
  the reference board). Until then it used to report zeroed temperatures. The KDE app shows a
  waiting banner meanwhile.
- Model gating via `R_UW_MODEL_ID` (generalise safely beyond the Gen9 reference board). The
  `tuxedoio` library refuses EC writes on any board whose model id is not in a known-models
  registry (`TuxedoIo::open`), so an unvalidated board runs read-only; the prober uses
  `TuxedoIo::open_unchecked` to validate new boards. The daemon logs the gating decision and
  surfaces `model_id` + `read_only` in its `STATUS` reply. See `docs/model-gating.md`.
- Simulated `tuxedo_io` EC test harness (`tuxedoio::sim`) with a per-model fixture table, plus a
  model-matrix test suite run in CI (`cargo test --workspace`). For each board it verifies the
  write-gate (only validated models are writable), fan-duty scaling, the `0x40` fan-ownership
  fix, performance-profile enforcement, and capability probes. A companion per-model **sysfs**
  matrix in the daemon (driven by the same fixtures) simulates `/sys` to check charging-profile
  presence-gating + enum validation and keyboard-backlight discovery + per-model
  `max_brightness`; plus unit tests for fan-curve interpolation and the safety floor.

### Fixed
- The daemon never steers the fan from an implausible CPU temperature (read error, <= 0 or
  >= 120 C). A 0 C read could switch the fan off while the CPU was hot. After three bad reads
  in a row it hands the fan back to the EC and resumes the curve once reads recover.
  `TUXEDO_DEBUG=1` logs the raw reads every tick.
- Fan revving irregularly after resume from hibernation. Hibernation resets the EC (performance
  profile lost, EC back on its own fan control) and the daemon never re-ran its EC setup. New
  `tuxedo-control-sleep.service` stops the daemon before sleep and starts it on resume, the
  pattern TCC uses in `tccd-sleep.service`. Shipped in the Arch package and the NixOS module.
- Fan on/off hunting near a curve's 0 -> first-duty step. With the CPU idling around the step,
  the fan started and stopped every minute or so. A spinning fan now needs 3 C more margin than
  `hysteresis_c` and 60 s of run time before it stops, and holds the curve's lowest duty until
  then. Temperature smoothing is a float EMA over 10 samples (was an integer EMA over 3, which
  also truncated and could sit several degrees below a steady reading).
- Fan-duty oscillation ("hunting"): the control loop read the instantaneous temperature each
  tick and rewrote the fan duty on every 1 °C of sensor noise, so the fan audibly ramped up and
  down. It now EMA-smooths the temperature (curve follows the trend, not spikes) and applies a
  duty deadband (small changes hold the current duty), so the fan settles and stays steady. The
  safety floor still uses the raw temperature, so real heat gets an immediate response. Measured:
  on the reference board the fan went from a change every few seconds to one change in 70 s at idle.

## [0.2.0] - 2026-06-29

### Added
- Module options `keyboard.backlight.brightness` and `charging.profile`, re-asserted by
  the daemon on every start (declarative keyboard backlight and battery charging profile).
- NixOS VM test (`checks.vm-test`): boots a VM with the module enabled and asserts the unit
  wiring, that tuxedo-rs/tailord is disabled, that the declarative options reach the daemon
  config, and that the daemon fails gracefully without hardware. Runs in GitHub CI.

### Changed
- GUI fan-curve editor now labels both axes: temperature in °C and fan duty in %, with
  per-tick units and "Temperature (°C)" / "Fan duty (%)" axis titles.

## [0.1.0] - 2026-06-27

### Added
- `tuxedoio` crate: wrappers over the `/dev/tuxedo_io` Uniwill ioctl interface (read
  temps/fan duty/mode, set fan duty, set performance profile, restore EC auto).
- `tuxedo-prober`: standalone tool to read temps/fan, drive fan duty, and toggle the
  `0x40` fan-ownership bit, used to validate the protocol on real hardware.
- `tuxedo-controld` daemon: temperature→duty fan curve with hysteresis, performance-
  profile control, a Unix control socket, and restores EC auto on `SIGTERM`.
- NixOS module `services.tuxedo-control` plus flake outputs (package, module, dev shell,
  checks, formatter).
- `tuxedo-tui`: live terminal dashboard.
- `tuxedo-gui`: libadwaita/GTK4 front-end with keyboard-backlight and charging-profile
  controls, dark-mode / follow-system colour scheme.
- Named performance profiles with their own fan curves (create/delete/default), built-in
  profiles modelled on TUXEDO Control Center, and import of TCC's exported profiles.
- Documentation: source-derived ioctl protocol map and root-cause analysis of the
  Uniwill-AMD fan bug.

### Fixed
- Loud-fan-at-idle bug on Uniwill-AMD boards: `uw_set_fan` silently ignores the
  requested duty unless bit `0x40` of EC RAM `0x0751` is clear. The daemon now clears
  `0x40` before every fan write.

### Known limitations
- Validated only on the reference machine (TUXEDO InfinityBook Pro AMD Gen9, Uniwill,
  `tuxedo_io` v0.3.9). Other boards are unverified. See "Supported hardware".
- The driver enforces a ~25% on-speed floor; a requested duty below ~12% becomes 0 (off).

[Unreleased]: https://github.com/AndrewDemsDS/tuxedo-control/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/AndrewDemsDS/tuxedo-control/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/AndrewDemsDS/tuxedo-control/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/AndrewDemsDS/tuxedo-control/releases/tag/v0.1.0
