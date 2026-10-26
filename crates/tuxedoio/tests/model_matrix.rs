//! Per-model matrix over the simulated EC harness (`tuxedoio::sim`). For every fixture in
//! `MODELS` this proves the model write-gate, the capability probes, and — only on the single
//! validated board — that fan/perf/auto writes reach the EC with the ownership-bit fix applied.
//! On every other (read-only) board it proves no write leaks to the simulated EC.

use std::io::ErrorKind;

use tuxedoio::sim::{mock, mock_unchecked, MODELS};
use tuxedoio::{pct_to_raw, raw_to_pct, PerfProfile, FAN_OWNERSHIP_BIT, W_UW_FANSPEED};

#[test]
fn model_matrix() {
    assert!(!MODELS.is_empty(), "fixture table must not be empty");

    for fix in MODELS {
        let (dev, ec) = mock(*fix);
        let label = fix.name;

        // 1. uniwill probe
        assert_eq!(
            dev.is_uniwill().unwrap(),
            fix.is_uniwill,
            "is_uniwill mismatch on {label}"
        );

        // 2. model id
        assert_eq!(
            dev.model_id().unwrap(),
            fix.model_id,
            "model_id mismatch on {label}"
        );

        // 3. THE gate: writes allowed iff the model is in the validated registry.
        assert_eq!(
            dev.write_allowed(),
            fix.is_validated(),
            "write-gate mismatch on {label}"
        );

        // 4. capability probes
        assert_eq!(
            dev.fans_off_available().unwrap(),
            fix.fans_off,
            "fans_off mismatch on {label}"
        );
        assert_eq!(
            dev.fans_min_speed().unwrap(),
            fix.fans_min_speed,
            "fans_min_speed mismatch on {label}"
        );
        assert_eq!(
            dev.profs_available().unwrap(),
            fix.profs_available,
            "profs_available mismatch on {label}"
        );
        let caps = dev.caps().unwrap();
        assert_eq!(caps.model_id, fix.model_id, "caps.model_id on {label}");
        assert_eq!(caps.fans_off, fix.fans_off, "caps.fans_off on {label}");
        assert_eq!(
            caps.fans_min_speed, fix.fans_min_speed,
            "caps.fans_min_speed on {label}"
        );
        assert_eq!(
            caps.profs_available, fix.profs_available,
            "caps.profs_available on {label}"
        );

        if fix.is_validated() {
            // 5. Writes are allowed: the EC must actually change.

            // The daemon's fix: ownership bit set first, then a fan write must clear it and land.
            ec.lock().unwrap().reg_0751 |= FAN_OWNERSHIP_BIT;
            dev.set_fan_pct(50).unwrap();
            {
                let st = ec.lock().unwrap();
                assert_eq!(st.fan1_raw, pct_to_raw(50), "fan1 not written on {label}");
                assert_eq!(st.fan2_raw, pct_to_raw(50), "fan2 not written on {label}");
                assert_eq!(
                    st.reg_0751 & FAN_OWNERSHIP_BIT,
                    0,
                    "ownership bit not cleared on {label}"
                );
            }

            // Perf profiles drive the 0x0751 profile bits.
            dev.set_perf(PerfProfile::PowerSave).unwrap();
            {
                let st = ec.lock().unwrap();
                assert_eq!(st.reg_0751 & 0xa0, 0xa0, "powersave bits on {label}");
                assert_eq!(st.last_perf, 1, "powersave last_perf on {label}");
            }
            dev.set_perf(PerfProfile::Overboost).unwrap();
            {
                let st = ec.lock().unwrap();
                assert_eq!(st.reg_0751 & 0x10, 0x10, "overboost bit on {label}");
                assert_eq!(st.last_perf, 3, "overboost last_perf on {label}");
            }
            dev.set_perf(PerfProfile::Enthusiast).unwrap();
            {
                let st = ec.lock().unwrap();
                assert_eq!(
                    st.reg_0751 & (0xa0 | 0x10),
                    0,
                    "enthusiast clears profile bits on {label}"
                );
                assert_eq!(st.last_perf, 2, "enthusiast last_perf on {label}");
            }

            dev.restore_auto().unwrap();
            assert!(ec.lock().unwrap().auto, "auto not restored on {label}");
        } else {
            // 6. Read-only: every write must be refused AND leave the EC untouched.
            let e = dev.set_fan_pct(50).unwrap_err();
            assert_eq!(
                e.kind(),
                ErrorKind::PermissionDenied,
                "set_fan_pct should be PermissionDenied on {label}"
            );
            assert!(
                dev.set_perf(PerfProfile::Enthusiast).is_err(),
                "set_perf should be refused on {label}"
            );
            assert!(
                dev.restore_auto().is_err(),
                "restore_auto should be refused on {label}"
            );

            let st = ec.lock().unwrap();
            assert_eq!(st.fan1_raw, 0, "fan1 leaked a write on {label}");
            assert_eq!(st.fan2_raw, 0, "fan2 leaked a write on {label}");
            assert!(!st.auto, "auto leaked a write on {label}");
            assert_eq!(st.last_perf, 0, "perf leaked a write on {label}");
        }
    }
}

/// Why the daemon clears 0x40 before every fan write: with the ownership bit set, a raw fan
/// write is silently dropped by the EC. `set_fan_pct` clears it first (via `ensure_manual`).
#[test]
fn bug_fix_requires_clearing_ownership() {
    // A validated-shaped fixture so the gate allows writes; mock_unchecked also bypasses it.
    let fix = MODELS
        .iter()
        .find(|f| f.is_validated())
        .copied()
        .expect("need a validated fixture");
    let (dev, ec) = mock_unchecked(fix);

    // Ownership grabbed by the EC: a raw fan write is ignored.
    ec.lock().unwrap().reg_0751 |= FAN_OWNERSHIP_BIT;
    dev.wr(W_UW_FANSPEED, pct_to_raw(60)).unwrap();
    assert_eq!(
        ec.lock().unwrap().fan1_raw,
        0,
        "raw fan write must be dropped while EC owns the fan"
    );

    // set_fan_pct clears ownership first, so now the write lands.
    dev.set_fan_pct(60).unwrap();
    assert_eq!(
        ec.lock().unwrap().fan1_raw,
        pct_to_raw(60),
        "set_fan_pct must land after clearing ownership"
    );
}

/// Percent<->raw scaling is monotone and round-trips within ±1.
#[test]
fn scaling_roundtrip() {
    assert_eq!(pct_to_raw(0), 0);
    assert_eq!(pct_to_raw(100), 0xc8);
    for p in [0, 25, 50, 75, 100] {
        let rt = raw_to_pct(pct_to_raw(p));
        assert!((rt - p).abs() <= 1, "roundtrip {p} -> {rt} drifted > 1");
    }
}

/// The EC rejects a profile a board does not expose: a 2-profile board refuses Overboost(3),
/// a 0-profile board refuses everything. Driven via mock_unchecked so the gate is out of the way.
#[test]
fn profile_enforcement() {
    let two = MODELS
        .iter()
        .find(|f| f.profs_available == 2)
        .copied()
        .expect("need a 2-profile fixture");
    let (dev, _ec) = mock_unchecked(two);
    dev.set_perf(PerfProfile::PowerSave).unwrap();
    dev.set_perf(PerfProfile::Enthusiast).unwrap();
    assert!(
        dev.set_perf(PerfProfile::Overboost).is_err(),
        "2-profile board must reject Overboost"
    );

    let zero = MODELS
        .iter()
        .find(|f| f.profs_available == 0)
        .copied()
        .expect("need a 0-profile fixture");
    let (dev, _ec) = mock_unchecked(zero);
    for p in [
        PerfProfile::PowerSave,
        PerfProfile::Enthusiast,
        PerfProfile::Overboost,
    ] {
        assert!(
            dev.set_perf(p).is_err(),
            "0-profile board must reject every profile"
        );
    }
}

/// tuxedo_io never fails a Uniwill read: an EC read error comes back as 0xfe. Clearing 0x40
/// from that would write 0xbe to 0x0751 (both profile bits plus junk), so the mode read must
/// reject it and the fan write must not touch the register.
#[test]
fn failed_mode_read_is_never_written_back() {
    let fix = MODELS
        .iter()
        .find(|f| f.is_validated())
        .copied()
        .expect("need a validated fixture");
    let (dev, ec) = mock_unchecked(fix);
    ec.lock().unwrap().reg_0751 = 0xfe;
    assert!(dev.mode().is_err());
    assert!(dev.set_fan_pct(60).is_err());
    let st = *ec.lock().unwrap();
    assert_eq!(st.reg_0751, 0xfe, "garbage mode must not be written back");
    assert_eq!(st.fan1_raw, 0);

    // Real values still pass, and an out-of-range fan read is rejected, not scaled to 127%.
    ec.lock().unwrap().reg_0751 = 0xa0 | FAN_OWNERSHIP_BIT;
    assert_eq!(dev.mode().unwrap(), 0xe0);
    dev.set_fan_pct(60).unwrap();
    assert_eq!(ec.lock().unwrap().reg_0751, 0xa0);
    ec.lock().unwrap().fan1_raw = 0xfe;
    assert!(dev.cpu_fan_pct().is_err());
}

/// The driver caches model id 0 when its one-time EC probe fails and re-probes on the next
/// identify. A transient failure must not leave the validated board read-only.
#[test]
fn transient_model_probe_failure_is_retried() {
    use tuxedoio::sim::{mock_with, EcState};
    let fix = MODELS
        .iter()
        .find(|f| f.is_validated())
        .copied()
        .expect("need a validated fixture");
    let st = EcState {
        model_probe_failures: 1,
        ..EcState::default()
    };
    let (dev, _ec) = mock_with(fix, st);
    assert!(dev.write_allowed(), "one failed probe must be retried");
    assert_eq!(dev.model_id().unwrap(), fix.model_id);

    // A probe that keeps failing stays read-only (fail safe).
    let st = EcState {
        model_probe_failures: 100,
        ..EcState::default()
    };
    let (dev, _ec) = mock_with(fix, st);
    assert!(!dev.write_allowed());
}
