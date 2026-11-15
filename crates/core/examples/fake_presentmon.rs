//! Stand-in for PresentMon used by the capture tests.
//!
//! Behaves like the real console tool as far as Blackbox is concerned:
//! * `--output_file PATH --session_name NAME ...` writes a CSV in the
//!   PresentMon 2.6 column layout until the session is terminated,
//! * `--session_name NAME --terminate_existing_session` ends that session.
//!
//! Its behaviour is read from `fake.cfg` next to the output file
//! (`app=`, `ft=` frame time in ms, `fail=1` to exit at once). The received
//! arguments are written to `args.txt` in the same folder.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn stop_file(session: &str) -> PathBuf {
    std::env::temp_dir().join(format!("fake-presentmon-{session}.stop"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let session = arg_value(&args, "--session_name").unwrap_or_else(|| "PresentMon".into());

    if args.iter().any(|a| a == "--terminate_existing_session") {
        let _ = fs::write(stop_file(&session), b"stop");
        return;
    }

    let Some(output) = arg_value(&args, "--output_file").map(PathBuf::from) else {
        eprintln!("error: --output_file missing");
        std::process::exit(2);
    };
    let dir = output.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _ = fs::write(dir.join("args.txt"), args.join("\n"));

    let cfg = fs::read_to_string(dir.join("fake.cfg")).unwrap_or_default();
    let get = |k: &str| cfg.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(str::trim).map(String::from);
    if get("fail").as_deref() == Some("1") {
        eprintln!("error: failed to start trace session (access denied)");
        std::process::exit(5);
    }
    let app = get("app").unwrap_or_else(|| "game.exe".into());
    let ft: f64 = get("ft").and_then(|v| v.parse().ok()).unwrap_or(5.0);

    let _ = fs::remove_file(stop_file(&session));
    let mut out = OpenOptions::new().create(true).write(true).truncate(true).open(&output).expect("output file");
    writeln!(
        out,
        "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,FrameType,CPUStartTime,MsCPUBusy,MsCPUWait,MsBetweenPresents,MsInPresentAPI"
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut t = 0.0;
    while Instant::now() < deadline {
        for _ in 0..50 {
            t += ft;
            writeln!(out, "{app},4242,0x1A2B,DXGI,0,0,Application,{t:.4},{:.4},0.1000,{ft:.4},0.0500", ft * 0.8).unwrap();
        }
        // Other programs present frames too; they must not count.
        for _ in 0..5 {
            writeln!(out, "dwm.exe,1000,0x0001,Other,1,0,Application,{t:.4},1.0,15.6,16.6667,0.0100").unwrap();
        }
        out.flush().unwrap();
        if stop_file(&session).exists() {
            let _ = fs::remove_file(stop_file(&session));
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
