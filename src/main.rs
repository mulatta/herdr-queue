use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use hq::list;
use hq::snapshot::Store;

const USAGE: &str = "usage:
  hq list [--json]   show every agent session's cache and usage state";

fn now() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn list_sessions(json: bool) -> Result<(), String> {
    let snaps = Store::default_dir()
        .map(|d| Store::new(d).all())
        .unwrap_or_default();
    if json {
        println!(
            "{}",
            serde_json::to_string(&snaps).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", list::table(&snaps, now()));
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["list"] => list_sessions(false),
        ["list", "--json"] => list_sessions(true),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hq: {e}");
            ExitCode::FAILURE
        }
    }
}
