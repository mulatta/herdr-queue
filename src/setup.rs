//! Connects `hq` to the agents: what each one needs, whether it has it, and how to change that.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::fmt::span;
use crate::snapshot::Store;

/// Written to pi's extension folder; compiled in so any install of `hq` can place it.
pub const PI_EXTENSION: &str = include_str!("../integrations/pi/hq.ts");
// The extension's first line, which tells a copy `hq` wrote from someone else's file.
const PI_MARKER: &str = "// Installed by hq setup";
// How often Claude Code re-runs the status line, and with it the recording.
const REFRESH_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Pi];

    /// The agent's command, which is also how it is named on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == name)
    }

    /// The one file `hq` touches for this agent, honouring the agent's own override.
    #[must_use]
    pub fn config_path(self) -> Option<PathBuf> {
        let dir = |var: &str, default: &str| {
            std::env::var_os(var)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(default)))
        };
        Some(match self {
            Self::Claude => dir("CLAUDE_CONFIG_DIR", ".claude")?.join("settings.json"),
            Self::Codex => dir("CODEX_HOME", ".codex")?.join("hooks.json"),
            Self::Pi => dir("PI_CODING_AGENT_DIR", ".pi/agent")?.join("extensions/hq.ts"),
        })
    }
}

#[must_use]
pub fn on_path(program: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Missing,
    Current,
    /// Installed by a different version of `hq`.
    Outdated,
    /// Something `hq` did not install sits where its entry goes.
    Foreign(String),
}

#[derive(Debug)]
pub enum Action {
    Write(String),
    Delete,
}

#[derive(Debug)]
pub struct Change {
    pub agent: Agent,
    pub path: PathBuf,
    pub summary: String,
    /// What the change would replace, when `hq` did not put it there.
    pub foreign: Option<String>,
    pub action: Action,
}

/// One agent's config as found on disk.
pub struct Found {
    pub agent: Agent,
    pub path: PathBuf,
    pub state: State,
    text: Option<String>,
    program: String,
}

/// How the agents should start `hq`: by name while `PATH` has it, else by this binary's own path.
#[must_use]
pub fn program() -> String {
    if on_path("hq").is_some() {
        return "hq".into();
    }
    let exe = std::env::current_exe().ok();
    // macOS reports the path as it was typed, `./` and all.
    let exe = exe.map(|p| {
        p.components()
            .filter(|c| *c != Component::CurDir)
            .collect::<PathBuf>()
    });
    exe.map_or_else(|| "hq".into(), |p| p.display().to_string())
}

fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "/._-+@%:,=".contains(c)
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `program`, quoted if a shell would otherwise split it.
fn shell_word(program: &str) -> String {
    if program.chars().all(is_plain) {
        program.into()
    } else {
        quote(program)
    }
}

// Whatever its directory and arguments, a command that runs `hq` is one `hq` installed.
fn is_ours(command: &Value) -> bool {
    let command = command.as_str().unwrap_or_default().trim_start();
    let word = match command.strip_prefix('\'') {
        Some(quoted) => quoted.split('\'').next(),
        None => command.split_whitespace().next(),
    };
    word.and_then(|w| Path::new(w).file_name()) == Some("hq".as_ref())
}

fn object(text: Option<&str>, path: &Path) -> Result<Map<String, Value>, String> {
    match text.map(serde_json::from_str::<Value>) {
        None => Ok(Map::new()),
        Some(Ok(Value::Object(map))) => Ok(map),
        Some(Ok(_)) => Err(format!("{} is not a JSON object", path.display())),
        Some(Err(e)) => Err(format!("{} is not valid JSON: {e}", path.display())),
    }
}

fn pretty(root: Map<String, Value>) -> String {
    let mut text = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    text.push('\n');
    text
}

// Separates `hq ingest claude` from the status line command it wraps.
const WRAPS: &str = " -- ";

/// The command that keeps recording and, if the user had a status line, keeps drawing it.
fn claude_command(program: &str, inner: Option<&str>) -> String {
    let base = format!("{} ingest claude", shell_word(program));
    match inner {
        None => base,
        // Plain words survive being passed as arguments; anything else needs its own shell.
        Some(inner) if inner.chars().all(|c| is_plain(c) || c == ' ') => {
            format!("{base}{WRAPS}{inner}")
        }
        Some(inner) => format!("{base}{WRAPS}sh -c {}", quote(inner)),
    }
}

/// The user's own status line command inside one `hq` wrapped, as they wrote it.
fn wrapped(command: &str) -> Option<String> {
    let inner = command.split_once(WRAPS)?.1.trim();
    Some(
        match inner
            .strip_prefix("sh -c '")
            .and_then(|q| q.strip_suffix('\''))
        {
            Some(quoted) => quoted.replace("'\\''", "'"),
            None => inner.into(),
        },
    )
}

fn codex_group(program: &str) -> Value {
    let command = format!("{} ingest codex", shell_word(program));
    json!({"hooks": [{"type": "command", "command": command}]})
}

fn pi_extension(program: &str) -> String {
    let path = Value::from(program).to_string();
    PI_EXTENSION.replacen("const HQ = \"hq\";", &format!("const HQ = {path};"), 1)
}

fn codex_is_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hooks| hooks.iter().any(|h| is_ours(&h["command"])))
}

/// The groups under `hooks.PostModelRequest`, or why they cannot be edited.
fn codex_groups(root: &Map<String, Value>, path: &Path) -> Result<Vec<Value>, String> {
    let odd = |what: &str| format!("{}: {what} has an unexpected shape", path.display());
    let events = match root.get("hooks") {
        None => return Ok(Vec::new()),
        Some(Value::Object(events)) => events,
        Some(_) => return Err(odd("hooks")),
    };
    match events.get("PostModelRequest") {
        None => Ok(Vec::new()),
        Some(Value::Array(groups)) => Ok(groups.clone()),
        Some(_) => Err(odd("hooks.PostModelRequest")),
    }
}

impl Found {
    /// # Errors
    /// Fails when the config exists but is not something `hq` can edit safely.
    pub fn inspect(agent: Agent) -> Result<Self, String> {
        let path = agent.config_path().ok_or("HOME is not set")?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let program = program();
        let state = match agent {
            Agent::Claude => match object(text.as_deref(), &path)?.get("statusLine") {
                None | Some(Value::Null) => State::Missing,
                Some(line) => match line["command"].as_str() {
                    None => {
                        return Err(format!(
                            "{}: statusLine has an unexpected shape",
                            path.display()
                        ));
                    }
                    Some(command) if !is_ours(&line["command"]) => State::Foreign(command.into()),
                    Some(command)
                        if command == claude_command(&program, wrapped(command).as_deref())
                            && line["type"] == "command"
                            && line["refreshInterval"].is_number() =>
                    {
                        State::Current
                    }
                    Some(_) => State::Outdated,
                },
            },
            Agent::Codex => {
                let groups = codex_groups(&object(text.as_deref(), &path)?, &path)?;
                let ours: Vec<_> = groups.iter().filter(|g| codex_is_ours(g)).collect();
                match ours.as_slice() {
                    [] => State::Missing,
                    [one] if **one == codex_group(&program) => State::Current,
                    _ => State::Outdated,
                }
            }
            Agent::Pi => match text.as_deref() {
                None => State::Missing,
                Some(text) if text == pi_extension(&program) => State::Current,
                Some(other) if other.starts_with(PI_MARKER) => State::Outdated,
                Some(_) => State::Foreign("an extension hq did not install".into()),
            },
        };
        Ok(Self {
            agent,
            path,
            state,
            text,
            program,
        })
    }

    fn change(&self, summary: &str, action: Action) -> Change {
        Change {
            agent: self.agent,
            path: self.path.clone(),
            summary: summary.into(),
            foreign: match &self.state {
                State::Foreign(what) => Some(what.clone()),
                _ => None,
            },
            action,
        }
    }

    /// The edit that brings this agent up to date, if it needs one.
    ///
    /// # Errors
    /// Fails when the config changed shape since it was inspected.
    pub fn install(&self) -> Result<Option<Change>, String> {
        let verb = match self.state {
            State::Current => return Ok(None),
            State::Missing => "add",
            State::Outdated => "update",
            State::Foreign(_) => "replace",
        };
        Ok(Some(match self.agent {
            Agent::Claude => {
                let mut root = object(self.text.as_deref(), &self.path)?;
                // Keys beside ours, such as padding, are the user's and stay.
                let mut line = match root.get("statusLine") {
                    Some(Value::Object(line)) => line.clone(),
                    _ => Map::new(),
                };
                // Claude Code runs one status line command, so the user's keeps drawing inside ours.
                let inner = match (&self.state, line.get("command").and_then(Value::as_str)) {
                    (State::Foreign(theirs), _) => Some(theirs.clone()),
                    (_, Some(ours)) => wrapped(ours),
                    _ => None,
                };
                let command = claude_command(&self.program, inner.as_deref());
                let verb = if matches!(self.state, State::Foreign(_)) {
                    "wrap"
                } else {
                    verb
                };
                line.insert("type".into(), "command".into());
                line.insert("command".into(), command.clone().into());
                // Without a timer the snapshot would go stale while the session sits idle.
                line.entry("refreshInterval").or_insert(REFRESH_SECS.into());
                root.insert("statusLine".into(), Value::Object(line));
                let change = self.change(
                    &format!("{verb} statusLine: {command}"),
                    Action::Write(pretty(root)),
                );
                // Wrapping loses nothing, so there is nothing to ask about a second time.
                Change {
                    foreign: None,
                    ..change
                }
            }
            Agent::Codex => {
                let mut root = object(self.text.as_deref(), &self.path)?;
                let mut groups = codex_groups(&root, &self.path)?;
                groups.retain(|g| !codex_is_ours(g));
                groups.push(codex_group(&self.program));
                let events = root.entry("hooks").or_insert_with(|| json!({}));
                events["PostModelRequest"] = Value::Array(groups);
                self.change(
                    &format!(
                        "{verb} PostModelRequest hook: {} ingest codex",
                        self.program
                    ),
                    Action::Write(pretty(root)),
                )
            }
            Agent::Pi => self.change(
                &format!("{verb} extension"),
                Action::Write(pi_extension(&self.program)),
            ),
        }))
    }

    /// The edit that takes out what `hq` installed, leaving everything else.
    ///
    /// # Errors
    /// Fails when the config changed shape since it was inspected.
    pub fn remove(&self) -> Result<Option<Change>, String> {
        if !matches!(self.state, State::Current | State::Outdated) {
            return Ok(None);
        }
        Ok(Some(match self.agent {
            Agent::Claude => {
                let mut root = object(self.text.as_deref(), &self.path)?;
                let inner = root["statusLine"]["command"].as_str().and_then(wrapped);
                let summary = if let (Some(inner), Some(Value::Object(line))) =
                    (inner, root.get_mut("statusLine"))
                {
                    if line.get("refreshInterval") == Some(&REFRESH_SECS.into()) {
                        line.shift_remove("refreshInterval");
                    }
                    line.insert("command".into(), inner.clone().into());
                    format!("unwrap statusLine: {inner}")
                } else {
                    root.shift_remove("statusLine");
                    "remove statusLine".into()
                };
                self.change(&summary, Action::Write(pretty(root)))
            }
            Agent::Codex => {
                let mut root = object(self.text.as_deref(), &self.path)?;
                let mut groups = codex_groups(&root, &self.path)?;
                groups.retain(|g| !codex_is_ours(g));
                if let Some(Value::Object(events)) = root.get_mut("hooks") {
                    if groups.is_empty() {
                        events.shift_remove("PostModelRequest");
                    } else {
                        events.insert("PostModelRequest".into(), Value::Array(groups));
                    }
                    if events.is_empty() {
                        root.shift_remove("hooks");
                    }
                }
                // A file with nothing left in it was only ever ours.
                let action = if root.is_empty() {
                    Action::Delete
                } else {
                    Action::Write(pretty(root))
                };
                self.change("remove PostModelRequest hook", action)
            }
            Agent::Pi => self.change("remove extension", Action::Delete),
        }))
    }
}

fn yes(input: &mut dyn BufRead) -> io::Result<bool> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// Shows the plan and returns the changes the user agreed to.
///
/// # Errors
/// Fails when nobody can be asked and `assume_yes` is not set, or on I/O errors.
pub fn confirm(
    changes: Vec<Change>,
    assume_yes: bool,
    interactive: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<Change>, String> {
    let io = |e: io::Error| e.to_string();
    let width = changes
        .iter()
        .map(|c| c.path.display().to_string().len())
        .max()
        .unwrap_or(0);
    for c in &changes {
        let replaced = c
            .foreign
            .as_ref()
            .map(|f| format!(" (currently: {f})"))
            .unwrap_or_default();
        let path = c.path.display().to_string();
        writeln!(
            out,
            "{:<7} {path:<width$}  {}{replaced}",
            c.agent.name(),
            c.summary
        )
        .map_err(io)?;
    }
    if assume_yes {
        return Ok(changes);
    }
    if !interactive {
        return Err(
            "not a terminal, so nothing was written; pass --yes to apply these changes".into(),
        );
    }
    write!(out, "Proceed? [y/N] ")
        .and_then(|()| out.flush())
        .map_err(io)?;
    if !yes(input).map_err(io)? {
        return Ok(Vec::new());
    }
    let mut kept = Vec::new();
    for c in changes {
        if let Some(current) = &c.foreign {
            write!(out, "{}: replace {current}? [y/N] ", c.agent.name())
                .and_then(|()| out.flush())
                .map_err(io)?;
            if !yes(input).map_err(io)? {
                continue;
            }
        }
        kept.push(c);
    }
    Ok(kept)
}

/// # Errors
/// Fails when the file or its directory cannot be written.
pub fn apply(change: &Change) -> io::Result<()> {
    // A config that is a symlink keeps being one; the edit lands on its target.
    let target = fs::canonicalize(&change.path).unwrap_or_else(|_| change.path.clone());
    match &change.action {
        Action::Delete => fs::remove_file(&change.path),
        Action::Write(text) => {
            let dir = target.parent().unwrap_or(Path::new("."));
            fs::create_dir_all(dir)?;
            let tmp = dir.join(format!(".hq-setup.{}.tmp", std::process::id()));
            fs::write(&tmp, text)?;
            if let Ok(meta) = fs::metadata(&target) {
                fs::set_permissions(&tmp, meta.permissions())?;
            }
            fs::rename(tmp, target)
        }
    }
}

/// A line per installed agent and whether anything needs attention.
#[must_use]
pub fn doctor(now: i64) -> (String, bool) {
    let mut out = String::new();
    let mut healthy = true;
    let snaps = Store::default_dir()
        .map(|d| Store::new(d).all())
        .unwrap_or_default();
    for agent in Agent::ALL {
        if on_path(agent.name()).is_none() {
            continue;
        }
        let status = match Found::inspect(agent).map(|f| f.state) {
            Ok(State::Current) => "current".to_owned(),
            Ok(State::Outdated) => "outdated; run `hq setup`".into(),
            Ok(State::Missing) => "not set up; run `hq setup`".into(),
            Ok(State::Foreign(what)) => {
                format!("not set up ({what} is in its place); run `hq setup`")
            }
            Err(e) => e,
        };
        healthy &= status == "current";
        // Connected but silent usually means the agent has not run its hook yet, or may not.
        let last = snaps
            .iter()
            .filter(|s| s.agent == agent.name())
            .map(|s| s.updated_at)
            .max();
        let report = match last {
            _ if status != "current" => String::new(),
            Some(at) => format!(" (last report {} ago)", span(now - at)),
            None => " (no report yet)".into(),
        };
        _ = writeln!(out, "{:<7} {status}{report}", agent.name());
    }
    (out, healthy)
}
