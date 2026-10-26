//! tuxedo-tui: a tiny live dashboard for TUXEDO Uniwill hardware (run as root).
//!
//! Read-only monitor + quick controls. Run it with the daemon stopped (otherwise the
//! daemon re-asserts its curve over any manual nudges within a tick).
//!
//!   q quit   a EC-auto   1/2/3 perf (powersave/enthusiast/overboost)   +/- nudge fan ±5%

use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute, queue,
    style::Print,
    terminal::{self, Clear, ClearType},
};
use std::io::{stdout, Write};
use std::time::Duration;
use tuxedoio::{PerfProfile, TuxedoIo, FAN_OWNERSHIP_BIT};

fn draw(d: &TuxedoIo, manual: Option<i32>) -> std::io::Result<()> {
    let mut out = stdout();
    let mode = d.mode().unwrap_or(-1);
    queue!(out, cursor::MoveTo(0, 0), Clear(ClearType::All))?;
    let line =
        |o: &mut std::io::Stdout, row, s: String| queue!(o, cursor::MoveTo(0, row), Print(s));
    line(
        &mut out,
        0,
        format!(
            "── TUXEDO control  (tuxedo_io {}) ──",
            d.version().unwrap_or_default()
        ),
    )?;
    line(
        &mut out,
        2,
        format!(
            "CPU temp   {:>3} C     fan {:>3}%",
            d.cpu_temp().unwrap_or(-1),
            d.cpu_fan_pct().unwrap_or(-1)
        ),
    )?;
    line(
        &mut out,
        3,
        format!(
            "GPU temp   {:>3} C     fan {:>3}%",
            d.gpu_temp().unwrap_or(-1),
            d.gpu_fan_pct().unwrap_or(-1)
        ),
    )?;
    line(
        &mut out,
        5,
        format!(
            "mode 0x0751 = {:#04x}   {}",
            mode,
            if mode & FAN_OWNERSHIP_BIT != 0 {
                "EC owns fan (manual ignored)"
            } else {
                "manual control OK"
            }
        ),
    )?;
    line(
        &mut out,
        6,
        format!(
            "manual fan override: {}",
            manual
                .map(|p| format!("{p}%"))
                .unwrap_or_else(|| "none".into())
        ),
    )?;
    line(
        &mut out,
        8,
        "q quit   a auto   1 powersave  2 enthusiast  3 overboost   +/- fan ±5%".to_string(),
    )?;
    out.flush()
}

fn main() -> std::io::Result<()> {
    let d = match TuxedoIo::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("open /dev/tuxedo_io: {e} (run as root?)");
            std::process::exit(1);
        }
    };
    terminal::enable_raw_mode()?;
    execute!(stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
    let mut manual: Option<i32> = None;
    let res = (|| -> std::io::Result<()> {
        loop {
            draw(&d, manual)?;
            if event::poll(Duration::from_millis(1000))? {
                if let Event::Key(k) = event::read()? {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('a') => {
                            let _ = d.restore_auto();
                            manual = None;
                        }
                        KeyCode::Char('1') => {
                            let _ = d.set_perf(PerfProfile::PowerSave);
                        }
                        KeyCode::Char('2') => {
                            let _ = d.set_perf(PerfProfile::Enthusiast);
                        }
                        KeyCode::Char('3') => {
                            let _ = d.set_perf(PerfProfile::Overboost);
                        }
                        KeyCode::Char('+') | KeyCode::Char('=') => {
                            let p =
                                (manual.unwrap_or(d.cpu_fan_pct().unwrap_or(0)) + 5).clamp(0, 100);
                            let _ = d.set_fan_pct(p);
                            manual = Some(p);
                        }
                        KeyCode::Char('-') => {
                            let p =
                                (manual.unwrap_or(d.cpu_fan_pct().unwrap_or(0)) - 5).clamp(0, 100);
                            let _ = d.set_fan_pct(p);
                            manual = Some(p);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    })();
    // A +/- nudge leaves the EC's custom fan table holding that duty (from 0 up to 115 C) and
    // nothing else is driving the fan while the daemon is stopped, so hand it back on exit.
    if manual.is_some() {
        let _ = d.restore_auto();
    }
    execute!(stdout(), terminal::LeaveAlternateScreen, cursor::Show)?;
    terminal::disable_raw_mode()?;
    res
}
