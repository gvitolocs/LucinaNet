//! heatpause: freeze a systemd user unit while the CPU is hot, thaw it when it cools.
//!
//! the thermal rule for background work (2026-10-26): background CPU work pauses at Tctl >= 90 °C. Run the work as a
//! unit (`systemd-run --user --unit=<name> -p CPUQuota=... <cmd>`), then `heatpause --unit <name>`.
//! It exits when the unit is gone. No dependencies: reads k10temp from /sys/class/hwmon.

use std::{env, fs, process::Command, thread, time::Duration};

fn tctl_millic() -> Option<i64> {
    for e in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let p = e.path();
        if fs::read_to_string(p.join("name")).ok()?.trim() != "k10temp" {
            continue;
        }
        // temp1 is Tctl on k10temp; check the label anyway.
        for i in 1..=8 {
            let label = fs::read_to_string(p.join(format!("temp{i}_label"))).unwrap_or_default();
            if label.trim() == "Tctl" {
                return fs::read_to_string(p.join(format!("temp{i}_input"))).ok()?.trim().parse().ok();
            }
        }
    }
    None
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).status().map(|s| s.success()).unwrap_or(false)
}

fn unit_active(unit: &str) -> bool {
    let out = Command::new("systemctl").args(["--user", "is-active", unit]).output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            let s = s.trim();
            s == "active" || s == "activating" || s == "reloading"
        }
        Err(_) => false,
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut unit = None;
    let (mut hot, mut cool, mut interval) = (90.0f64, 85.0f64, 5u64);
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--unit" => unit = Some(v),
            "--hot" => hot = v.parse().expect("--hot <°C>"),
            "--cool" => cool = v.parse().expect("--cool <°C>"),
            "--interval" => interval = v.parse().expect("--interval <s>"),
            "-h" | "--help" => {
                println!("heatpause --unit <user unit> [--hot 90] [--cool 85] [--interval 5]");
                return;
            }
            o => panic!("unknown argument {o}"),
        }
        i += 2;
    }
    let unit = unit.expect("--unit <systemd user unit> is required");
    assert!(cool < hot, "--cool must be below --hot");
    // A watcher started (or restarted) on a unit that is already frozen must know it, or it never thaws it.
    let mut frozen = Command::new("systemctl")
        .args(["--user", "show", "-p", "FreezerState", "--value", &unit])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "frozen")
        .unwrap_or(false);
    let mut pauses = 0u32;
    while unit_active(&unit) {
        if let Some(m) = tctl_millic() {
            let c = m as f64 / 1000.0;
            if !frozen && c >= hot {
                if systemctl(&["freeze", &unit]) {
                    frozen = true;
                    pauses += 1;
                    eprintln!("heatpause: Tctl {c:.1} °C >= {hot}: froze {unit} (pause {pauses})");
                }
            } else if frozen && c <= cool {
                if systemctl(&["thaw", &unit]) {
                    frozen = false;
                    eprintln!("heatpause: Tctl {c:.1} °C <= {cool}: thawed {unit}");
                }
            }
        }
        thread::sleep(Duration::from_secs(interval));
    }
    if frozen {
        systemctl(&["thaw", &unit]);
    }
    eprintln!("heatpause: {unit} finished after {pauses} pauses");
}
