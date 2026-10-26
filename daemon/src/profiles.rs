//! Named performance profiles: TCC-style bundles of {perf mode, fan curve, kbd, charge}.
//!
//! Stored at /var/lib/tuxedo-control/profiles.json as { active, default, profiles[] }.
//! Built-ins mirror TUXEDO Control Center's modern defaults (Max Energy Save / Quiet /
//! Office / High Performance), mapping TCC's fan presets (Silent/Quiet/Balanced) +
//! odmProfile (power_save/enthusiast/overboost) onto our model. Also imports TCC's
//! exported profiles (a JSON array of ITccProfile).

use serde::{Deserialize, Serialize};

pub const STORE_PATH: &str = "/var/lib/tuxedo-control/profiles.json";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub perf: String,           // power_save | enthusiast | overboost
    pub curve: Vec<(i32, i32)>, // sparse (temp °C, duty %), ascending
    #[serde(default)]
    pub kbd: Option<i32>,
    #[serde(default)]
    pub charge: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Store {
    pub active: String,
    pub default: String,
    pub profiles: Vec<Profile>,
}

impl Store {
    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }
    pub fn active_profile(&self) -> Option<&Profile> {
        self.get(&self.active)
    }
}

// ---- TCC fan-curve presets (sparse approximations of TCC's per-degree tables) ----
fn fan(name: &str) -> Vec<(i32, i32)> {
    match name {
        "Silent" => vec![(0, 0), (60, 0), (61, 20), (70, 30), (85, 65), (100, 100)],
        "Quiet" => vec![(0, 0), (50, 0), (51, 20), (70, 33), (85, 70), (100, 100)],
        "Balanced" => vec![
            (0, 0),
            (45, 0),
            (46, 20),
            (50, 20),
            (70, 50),
            (85, 75),
            (100, 100),
        ],
        "Cool" => vec![
            (0, 0),
            (39, 0),
            (40, 20),
            (50, 25),
            (70, 50),
            (85, 85),
            (100, 100),
        ],
        "Freezy" => vec![(0, 20), (50, 40), (70, 55), (85, 85), (100, 100)],
        _ => vec![(0, 0), (50, 0), (62, 24), (80, 60), (90, 100)],
    }
}

/// Built-in profiles can't be deleted (they're re-seeded on load anyway).
pub fn is_builtin(name: &str) -> bool {
    builtins().iter().any(|p| p.name == name)
}

/// Fan safety floor: minimum duty % the fan must run at a given temperature, regardless of
/// the profile's curve. Protects against an edited/imported curve that would leave the fan
/// too low while hot. The daemon loop enforces it, and the GUI editor mirrors it.
pub fn safety_floor(temp: i32) -> i32 {
    match temp {
        t if t >= 90 => 80,
        t if t >= 85 => 60,
        t if t >= 80 => 45,
        t if t >= 75 => 30,
        _ => 0, // below 75 °C the curve may be as quiet as it likes (incl. fan off)
    }
}

/// TCC's modern default profiles, adapted (TDP/EPP n/a on this board).
pub fn builtins() -> Vec<Profile> {
    vec![
        Profile {
            name: "Max Energy Save".into(),
            perf: "power_save".into(),
            curve: fan("Silent"),
            kbd: Some(0),
            charge: None,
        },
        Profile {
            name: "Quiet".into(),
            perf: "power_save".into(),
            curve: fan("Quiet"),
            kbd: None,
            charge: None,
        },
        Profile {
            name: "Office".into(),
            perf: "enthusiast".into(),
            curve: fan("Quiet"),
            kbd: None,
            charge: None,
        },
        Profile {
            name: "High Performance".into(),
            perf: "overboost".into(),
            curve: fan("Balanced"),
            kbd: None,
            charge: None,
        },
    ]
}

pub fn default_store() -> Store {
    let p = builtins();
    Store {
        active: "Quiet".into(),
        default: "Quiet".into(),
        profiles: p,
    }
}

pub fn load() -> Store {
    match std::fs::read_to_string(STORE_PATH)
        .ok()
        .and_then(|s| serde_json::from_str::<Store>(&s).ok())
    {
        Some(mut s) => {
            // Ensure built-ins are always present (re-seed any the user removed by name).
            for b in builtins() {
                if !s.profiles.iter().any(|p| p.name == b.name) {
                    s.profiles.push(b);
                }
            }
            if s.get(&s.active.clone()).is_none() {
                s.active = s.default.clone();
            }
            s
        }
        None => default_store(),
    }
}

pub fn save(s: &Store) -> std::io::Result<()> {
    save_to(std::path::Path::new(STORE_PATH), s)
}

/// Write via a synced temp file and rename. A plain truncate-and-write left an empty or partial
/// file if power failed or a hibernate never resumed mid-write, and `load` then silently fell
/// back to the built-ins, dropping every user profile.
fn save_to(path: &std::path::Path, s: &Store) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(serde_json::to_string_pretty(s)?.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

// ---- TCC import ----
#[derive(Deserialize)]
struct TccFanEntry {
    temp: i32,
    speed: i32,
}
#[derive(Deserialize)]
struct TccCustomCurve {
    #[serde(default, rename = "tableCPU")]
    table_cpu: Vec<TccFanEntry>,
}
#[derive(Deserialize)]
struct TccFan {
    #[serde(default, rename = "fanProfile")]
    fan_profile: Option<String>,
    #[serde(default, rename = "customFanCurve")]
    custom_fan_curve: Option<TccCustomCurve>,
}
#[derive(Deserialize)]
struct TccOdm {
    #[serde(default)]
    name: Option<String>,
}
#[derive(Deserialize)]
struct TccProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    fan: Option<TccFan>,
    #[serde(default, rename = "odmProfile")]
    odm_profile: Option<TccOdm>,
}

/// Parse a TCC profiles export (JSON array of ITccProfile) into our Profile list.
pub fn import_tcc(json: &str) -> Result<Vec<Profile>, String> {
    let tcc: Vec<TccProfile> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(tcc
        .into_iter()
        .filter_map(|t| {
            let name = t.name?;
            let perf = t
                .odm_profile
                .and_then(|o| o.name)
                .filter(|n| ["power_save", "enthusiast", "overboost"].contains(&n.as_str()))
                .unwrap_or_else(|| "enthusiast".into());
            // Prefer a named TCC fan preset; else a custom curve; else Balanced.
            let curve = match t.fan {
                Some(f) => {
                    if let Some(fp) = f.fan_profile.as_deref().filter(|n| *n != "Custom") {
                        fan(fp)
                    } else if let Some(c) = f.custom_fan_curve {
                        let pts: Vec<(i32, i32)> =
                            c.table_cpu.into_iter().map(|e| (e.temp, e.speed)).collect();
                        if pts.is_empty() {
                            fan("Balanced")
                        } else {
                            pts
                        }
                    } else {
                        fan("Balanced")
                    }
                }
                None => fan("Balanced"),
            };
            Some(Profile {
                name,
                perf,
                curve,
                kbd: None,
                charge: None,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_replaces_the_store_atomically() {
        let dir = std::env::temp_dir().join(format!("tuxedo-store-{}", std::process::id()));
        let path = dir.join("profiles.json");
        let mut s = default_store();
        save_to(&path, &s).unwrap();
        s.active = "Office".into();
        save_to(&path, &s).unwrap();
        let back: Store = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.active, "Office");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
