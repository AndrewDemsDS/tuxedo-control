//! tuxedo-prober: exercise the /dev/tuxedo_io Uniwill interface (run as root).
//! Phase-2 validation tool; thin CLI over the `tuxedoio` crate.

use tuxedoio::*;

fn cmd_info(d: &TuxedoIo) -> std::io::Result<()> {
    let mode = d.rd(R_UW_MODE)?; // raw: the prober must show unexpected bits too
    println!("module version      : {}", d.version().unwrap_or_default());
    println!("uniwill hardware    : {}", d.rd(R_HWCHECK_UW)?);
    println!("model id            : {:#x}", d.rd(R_UW_MODEL_ID)?);
    println!(
        "mode reg 0x0751     : {:#04x}  (fan-ownership bit 0x40 = {})",
        mode,
        if mode & FAN_OWNERSHIP_BIT != 0 {
            "SET -> manual fan IGNORED"
        } else {
            "clear -> manual OK"
        }
    );
    println!("fans-off available  : {}", d.rd(R_UW_FANS_OFF_AVAILABLE)?);
    println!("fans min speed %    : {}", d.rd(R_UW_FANS_MIN_SPEED)?);
    println!("perf profiles avail : {}", d.rd(R_UW_PROFS_AVAILABLE)?);
    println!("CPU temp            : {} C", d.cpu_temp()?);
    println!("GPU temp            : {} C", d.gpu_temp()?);
    println!("CPU fan duty        : {}%", d.cpu_fan_pct()?);
    println!("GPU fan duty        : {}%", d.gpu_fan_pct()?);
    Ok(())
}

fn cmd_watch(d: &TuxedoIo) -> std::io::Result<()> {
    println!("ctrl-c to stop");
    loop {
        let mode = d.rd(R_UW_MODE)?; // raw: the prober must show unexpected bits too
        println!(
            "cpu {}C fan {}%  | gpu {}C fan {}%  | 0x0751={:#04x}{}",
            d.cpu_temp()?,
            d.cpu_fan_pct()?,
            d.gpu_temp()?,
            d.gpu_fan_pct()?,
            mode,
            if mode & FAN_OWNERSHIP_BIT != 0 {
                " (EC owns fan)"
            } else {
                ""
            }
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn usage() {
    eprintln!(
        "tuxedo-prober (run as root)\n\
        \n  info             read version, mode, temps, fan duty\
        \n  set <0-100>      set both fans to a duty %, clearing 0x40 first\
        \n  auto             restore EC automatic fan control\
        \n  perf <1|2|3>     set perf profile (1=powersave 2=enthusiast 3=overboost)\
        \n  mode-get         read EC RAM 0x0751\
        \n  mode-set <hex>   write EC RAM 0x0751 (e.g. 0x00)\
        \n  watch            loop temps + fan duty"
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The prober is the source-first validation tool for new boards, so it bypasses the
    // model write-gate (reads first, then writes with `auto` as the bail-out).
    let d = match TuxedoIo::open_unchecked() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("open /dev/tuxedo_io: {e} (run as root?)");
            std::process::exit(1);
        }
    };
    let r = match args.get(1).map(String::as_str) {
        Some("info") | None => cmd_info(&d),
        Some("set") => match args.get(2).and_then(|s| s.parse().ok()) {
            Some(p) => d.set_fan_pct(p).map(|_| println!("set both fans -> {p}%")),
            None => {
                usage();
                std::process::exit(2);
            }
        },
        Some("auto") => d.restore_auto().map(|_| println!("restored EC auto fan")),
        Some("perf") => match args.get(2).and_then(|s| s.parse::<i32>().ok()) {
            Some(p) => d
                .wr(W_UW_PERF_PROF, p)
                .map(|_| println!("perf profile -> {p}")),
            None => {
                usage();
                std::process::exit(2);
            }
        },
        Some("mode-get") => d.rd(R_UW_MODE).map(|m| println!("0x0751 = {:#04x}", m)),
        Some("mode-set") => match args
            .get(2)
            .and_then(|s| i32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        {
            Some(v) => d
                .wr(W_UW_MODE, v)
                .map(|_| println!("wrote 0x0751 = {:#04x}", v)),
            None => {
                usage();
                std::process::exit(2);
            }
        },
        Some("watch") => cmd_watch(&d),
        _ => {
            usage();
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("ioctl error: {e}");
        std::process::exit(1);
    }
}
