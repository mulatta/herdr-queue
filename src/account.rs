//! Each account's rate limits, asked of the agent's own program so that `hq` never holds a token.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::snapshot::{Limit, Store, VERSION};

/// Both programs answer from an endpoint that throttles hard; nothing asks more often than this.
const MIN_INTERVAL_SECS: i64 = 300;
const MAX_INTERVAL_SECS: i64 = 3600;
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(30);
const CODEX_START_TIMEOUT: Duration = Duration::from_secs(15);
const CODEX_REPLY_TIMEOUT: Duration = Duration::from_secs(20);

pub const AGENTS: [&str; 2] = ["claude", "codex"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub v: u32,
    pub agent: String,
    /// When `limits` were last read; a failed attempt leaves them and this untouched.
    pub updated_at: Option<i64>,
    pub attempted_at: i64,
    /// Attempts that failed since the last answer; each one doubles the wait.
    pub failures: u32,
    pub error: Option<String>,
    pub plan: Option<String>,
    pub limits: Vec<Limit>,
}

impl Account {
    fn due(&self, now: i64) -> bool {
        let wait = MIN_INTERVAL_SECS
            .saturating_mul(1_i64 << self.failures.min(8))
            .min(MAX_INTERVAL_SECS);
        now - self.attempted_at >= wait
    }
}

/// Days since the epoch for a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn year_of(epoch: i64) -> i64 {
    let z = epoch.div_euclid(86400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // Years here start in March, so January and February belong to the next one.
    yoe + era * 400 + i64::from(doy >= 306)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds into the day for `9:40am` or `7pm`.
fn clock(text: &str) -> Option<i64> {
    let (digits, pm) = match text.strip_suffix("am") {
        Some(d) => (d, false),
        None => (text.strip_suffix("pm")?, true),
    };
    let (hour, minute) = digits.split_once(':').unwrap_or((digits, "0"));
    let (hour, minute): (i64, i64) = (hour.parse().ok()?, minute.parse().ok()?);
    if !(1..=12).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    Some((hour % 12 + if pm { 12 } else { 0 }) * 3600 + minute * 60)
}

/// The epoch of `Nov 12 at 9:40am (UTC)` or `9:40am (UTC)`: the text carries no year, and
/// only UTC can be converted without a time zone database.
fn reset_at(text: &str, now: i64) -> Option<i64> {
    let text = text.trim().strip_suffix("(UTC)")?.trim();
    let Some((date, time)) = text.split_once(" at ") else {
        let today = now.div_euclid(86400) * 86400 + clock(text)?;
        return Some(if today > now { today } else { today + 86400 });
    };
    let (month, day) = date.trim().split_once(' ')?;
    let month = i64::try_from(MONTHS.iter().position(|m| *m == month)?).ok()? + 1;
    let day: i64 = day.trim().parse().ok()?;
    let seconds = clock(time.trim())?;
    let at = |year| days_from_civil(year, month, day) * 86400 + seconds;
    let this_year = at(year_of(now));
    // A reset lies ahead; one that reads as long past belongs to next year.
    Some(if this_year < now - 180 * 86400 {
        at(year_of(now) + 1)
    } else {
        this_year
    })
}

/// The windows in what `claude -p /usage` prints for people.
///
/// # Errors
/// Fails when no line reads as a window, as when Claude Code asks for a login instead.
pub fn claude_limits(output: &str, now: i64) -> Result<Vec<Limit>, String> {
    let limits: Vec<Limit> = output
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.trim().strip_prefix("Current ")?.split_once(": ")?;
            let (used, rest) = rest.split_once("% used")?;
            let resets_at = rest
                .split_once("resets ")
                .and_then(|(_, when)| reset_at(when, now));
            // Claude Code names its windows instead of giving their length.
            let (kind, scope, window_minutes) = match name {
                "session" => ("five_hour", None, Some(300)),
                "week (all models)" => ("seven_day", None, Some(10080)),
                _ => match name
                    .strip_prefix("week (")
                    .and_then(|n| n.strip_suffix(')'))
                {
                    Some(model) => ("seven_day", Some(model), Some(10080)),
                    None => (name, None, None),
                },
            };
            Some(Limit {
                kind: kind.into(),
                scope: scope.map(str::to_owned),
                window_minutes,
                used_pct: used.trim().parse().ok()?,
                resets_at,
            })
        })
        .collect();
    if limits.is_empty() {
        let first = output.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!("no usage lines in claude's answer: {first}"));
    }
    Ok(limits)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Slot {
    used_percent: f64,
    window_duration_mins: Option<u32>,
    resets_at: Option<i64>,
}

/// The windows and plan in the result of Codex's `account/rateLimits/read`.
///
/// # Errors
/// Fails when the result has no rate limits for Codex.
pub fn codex_limits(result: &Value) -> Result<(Vec<Limit>, Option<String>), String> {
    let limits = match &result["rateLimitsByLimitId"]["codex"] {
        Value::Null => &result["rateLimits"],
        by_id => by_id,
    };
    if !limits.is_object() {
        return Err("no rate limits in codex's answer".into());
    }
    // A plan with one window leaves the other slot null.
    let mut windows: Vec<Limit> = ["primary", "secondary"]
        .iter()
        .filter_map(|slot| {
            let w: Slot = serde_json::from_value(limits[slot].clone()).ok()?;
            Some(Limit {
                kind: (*slot).into(),
                scope: None,
                window_minutes: w.window_duration_mins,
                used_pct: w.used_percent,
                resets_at: w.resets_at,
            })
        })
        .collect();
    windows.sort_by_key(|l| l.window_minutes.unwrap_or(u32::MAX));
    Ok((windows, limits["planType"].as_str().map(str::to_owned)))
}

fn lines(stdout: impl Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn ask_claude() -> Result<String, String> {
    let mut child = Command::new("claude")
        // Without persistence each question would leave a session file behind.
        .args(["-p", "/usage", "--no-session-persistence"])
        // Reset times are printed in the local zone, which only UTC lets us convert.
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("claude: {e}"))?;
    let rx = lines(child.stdout.take().ok_or("claude: no stdout")?);
    let deadline = Instant::now() + CLAUDE_TIMEOUT;
    let mut output = String::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                stop(child);
                return Err("claude: no answer in time".into());
            }
        }
    }
    let _ = child.wait();
    Ok(output)
}

/// The reply to request `id`; the server also sends notifications nobody asked for.
fn reply(rx: &Receiver<String>, id: u64, timeout: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "codex: no answer in time".to_owned())?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["id"].as_u64() == Some(id) {
            return match message.get("result") {
                Some(result) => Ok(result.clone()),
                None => Err(format!("codex: {}", message["error"])),
            };
        }
    }
}

fn ask_codex() -> Result<Value, String> {
    let mut child = Command::new("codex")
        // The server only answers questions here; it should not be able to do more.
        .args(["-s", "read-only", "-a", "never", "app-server"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("codex: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("codex: no stdin")?;
    let rx = lines(child.stdout.take().ok_or("codex: no stdout")?);
    let mut send = |message: Value| writeln!(stdin, "{message}").map_err(|e| format!("codex: {e}"));
    let result = (|| {
        send(json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "hq", "version": env!("CARGO_PKG_VERSION")}
        }}))?;
        reply(&rx, 1, CODEX_START_TIMEOUT)?;
        send(json!({"method": "initialized"}))?;
        send(json!({"id": 2, "method": "account/rateLimits/read", "params": {}}))?;
        reply(&rx, 2, CODEX_REPLY_TIMEOUT)
    })();
    stop(child);
    result
}

fn ask(agent: &str, now: i64) -> Result<(Vec<Limit>, Option<String>), String> {
    match agent {
        "claude" => Ok((claude_limits(&ask_claude()?, now)?, None)),
        "codex" => codex_limits(&ask_codex()?),
        other => Err(format!("unknown agent: {other}")),
    }
}

/// Asks `agent`'s program for its limits unless it was asked recently, and records the answer.
///
/// # Errors
/// Fails when the state directory cannot be written; a failed question is recorded, not returned.
pub fn refresh(store: &Store, agent: &str, now: i64) -> std::io::Result<()> {
    let prev = store.read_account(agent);
    if prev.as_ref().is_some_and(|p| !p.due(now)) {
        return Ok(());
    }
    let mut account = prev.unwrap_or_else(|| Account {
        v: VERSION,
        agent: agent.into(),
        updated_at: None,
        attempted_at: now,
        failures: 0,
        error: None,
        plan: None,
        limits: Vec::new(),
    });
    // Claimed before asking, so a second caller meanwhile does not ask too.
    account.attempted_at = now;
    store.write_account(&account)?;
    match ask(agent, now) {
        Ok((limits, plan)) => {
            account.updated_at = Some(now);
            account.failures = 0;
            account.error = None;
            account.plan = plan;
            account.limits = limits;
        }
        Err(e) => {
            account.failures = account.failures.saturating_add(1);
            account.error = Some(e);
        }
    }
    store.write_account(&account)
}
