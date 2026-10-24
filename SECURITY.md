# Security Policy

Tuxedo Control ships a **daemon that runs as root** and writes to a laptop's
embedded controller (EC) through the `tuxedo_io` kernel ioctl interface
(`/dev/tuxedo_io`, EC RAM offsets such as `0x0751`, raw fan-duty registers). A bug or a
crafted configuration could set an unsafe fan state, write unexpected EC registers, or
be leveraged for local privilege escalation. I take reports in this area seriously.

## Reporting a vulnerability

**Please do not report security issues in public issues, pull requests, or
discussions.** Instead, use one of:

- **GitHub Security Advisories**: the "Report a vulnerability" button under the repo's
  **Security** tab (preferred).
- **Email**: `hello@andreasincode.com`.

Please include the affected component (`tuxedoio`, `tuxedo-controld`, `tuxedo-prober`,
`tuxedo-gui`, the NixOS module) and version/commit, your hardware (model, board name,
`tuxedo_io` version, kernel), the impact, and reproduction steps. If a report can cause
physical risk (e.g. forcing the fan off under load), say so prominently.

## Supported versions

Pre-1.0; only the latest release and `main` receive security fixes.

| Version | Supported |
|---|---|
| `main` (latest) | yes |
| latest `0.x` release | yes |
| older releases | no, please upgrade |

## Scope

In scope: privilege escalation via the daemon/ioctl/IPC; memory-safety bugs in the
`unsafe` FFI layer; input/config parsing that can write unintended EC registers or unsafe
fan states; the NixOS module granting more privilege than necessary.

Out of scope: vulnerabilities in the upstream `tuxedo-drivers` kernel modules (report
those to TUXEDO upstream); issues requiring an attacker who is already root (the daemon
runs as root by design); physical-access attacks against firmware; generic thermal
behaviour unrelated to a code defect.

## Not a vulnerability

If the tool merely misbehaves on your hardware (wrong fan curve, unsupported board) with
no security/safety angle, that's a normal bug report. Use the public issue tracker.
