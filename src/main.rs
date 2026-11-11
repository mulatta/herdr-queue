use std::io::{Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use hq::claude::{self, Status};
use hq::codex::{self, Hook, Rollout};
use hq::snapshot::{Snapshot, Store, valid_terminal};
use hq::{events, list};

const USAGE: &str = "usage:
  hq ingest claude [-- command...]
                     record Claude Code's status line JSON from stdin, then run command on it
  hq ingest codex    record Codex's PostModelRequest hook JSON from stdin
  hq list [--json]   show every agent session's cache and usage state";

fn now() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs).unwrap_or(i64::MAX)
}

fn stdin() -> Result<String, String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())?;
    Ok(input)
}

/// The store and this terminal's id; outside herdr no terminal owns a snapshot.
fn home() -> Option<(Store, String)> {
    let terminal = std::env::var("HC_TERMINAL_ID")
        .ok()
        .filter(|t| valid_terminal(t))?;
    Some((Store::new(Store::default_dir()?), terminal))
}

fn record(store: &Store, prev: Option<&Snapshot>, next: &Snapshot) -> Result<(), String> {
    store.write(next).map_err(|e| format!("snapshot: {e}"))?;
    if let Some(event) = prev.and_then(|prev| events::cache_miss(prev, next)) {
        events::append(&store.events_path(), &event).map_err(|e| format!("event log: {e}"))?;
    }
    Ok(())
}

/// Hands the status line input to the command `hq` was wrapped around, which draws the line.
fn pass_through(command: &[&str], input: &str) -> Result<(), String> {
    let mut child = Command::new(command[0])
        .args(&command[1..])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", command[0]))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A command that exits without reading its input has still done its job.
        let _ = stdin.write_all(input.as_bytes());
    }
    let status = child.wait().map_err(|e| format!("{}: {e}", command[0]))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{}: {status}", command[0]))
    }
}

fn record_claude(input: &str) -> Result<(), String> {
    let status = Status::parse(input).map_err(|e| format!("status line input: {e}"))?;
    if let Some((store, terminal)) = home() {
        let prev = store.read(&terminal);
        record(
            &store,
            prev.as_ref(),
            &claude::snapshot(&status, &terminal, now()),
        )?;
    }
    Ok(())
}

fn ingest_claude(rest: &[&str]) -> Result<(), String> {
    let command = match rest {
        [] => None,
        ["--", command @ ..] if !command.is_empty() => Some(command),
        _ => return Err("expected `hq ingest claude [-- command...]`".into()),
    };
    let input = stdin()?;
    let recorded = record_claude(&input);
    // The user looks at the status line, not the snapshot, so it is drawn whatever happened above.
    if let Some(command) = command {
        pass_through(command, &input)?;
    }
    recorded
}

fn ingest_codex() -> Result<(), String> {
    let hook = Hook::parse(&stdin()?).map_err(|e| format!("hook input: {e}"))?;
    let Some((store, terminal)) = home() else {
        return Ok(());
    };
    let prev = store.read(&terminal);
    let rollout = hook.transcript_path.as_deref().and_then(Rollout::read);
    match codex::snapshot(&hook, &terminal, now(), prev.as_ref(), rollout.as_ref()) {
        Some(next) => record(&store, prev.as_ref(), &next),
        None => Ok(()),
    }
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
        ["ingest", "claude", rest @ ..] => ingest_claude(rest),
        ["ingest", "codex"] => ingest_codex(),
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
