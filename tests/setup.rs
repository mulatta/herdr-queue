use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hq::setup::{self, Action, Agent, Change};
use serde_json::{Value, json};

struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    /// A private home whose PATH holds only the named agents and `hq` itself.
    fn new(name: &str, agents: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!("hq-setup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (home, bin) = (root.join("home"), root.join("bin"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        for agent in agents {
            let path = bin.join(agent);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(env!("CARGO_BIN_EXE_hq"), bin.join("hq")).unwrap();
        Self { home, bin }
    }

    fn hq(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_hq"))
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap();
        (out.status.code().unwrap(), text)
    }

    fn claude(&self) -> PathBuf {
        self.home.join(".claude/settings.json")
    }

    fn codex(&self) -> PathBuf {
        self.home.join(".codex/hooks.json")
    }

    fn pi(&self) -> PathBuf {
        self.home.join(".pi/agent/extensions/hq.ts")
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

const STATUS_LINE: &str = r#"{"type":"command","command":"hq ingest claude","refreshInterval":30}"#;

#[test]
fn installs_into_every_agent_on_path_and_is_idempotent() {
    let sb = Sandbox::new("all", &["claude", "codex", "pi"]);
    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(".claude/settings.json"), "{out}");

    assert_eq!(
        read_json(&sb.claude())["statusLine"],
        serde_json::from_str::<Value>(STATUS_LINE).unwrap()
    );
    assert_eq!(
        read_json(&sb.codex()),
        json!({"hooks": {"PostModelRequest": [{"hooks": [{"type": "command", "command": "hq ingest codex"}]}]}})
    );
    assert_eq!(fs::read_to_string(sb.pi()).unwrap(), setup::PI_EXTENSION);

    let before = fs::read_to_string(sb.claude()).unwrap();
    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("nothing to do"), "{out}");
    assert_eq!(fs::read_to_string(sb.claude()).unwrap(), before);
}

#[test]
fn only_agents_on_path_are_set_up_unless_named() {
    let sb = Sandbox::new("path", &["pi"]);
    let (code, _) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0);
    assert!(sb.pi().exists());
    assert!(!sb.claude().exists() && !sb.codex().exists());

    let (code, _) = sb.hq(&["setup", "codex", "--yes"]);
    assert_eq!(code, 0);
    assert!(sb.codex().exists());
}

#[test]
fn refuses_to_write_without_a_terminal_or_yes() {
    let sb = Sandbox::new("refuse", &["claude"]);
    let (code, out) = sb.hq(&["setup"]);
    assert_ne!(code, 0);
    assert!(out.contains("--yes"), "{out}");
    assert!(!sb.claude().exists());
}

#[test]
fn keeps_other_settings_and_their_order() {
    let sb = Sandbox::new("keep", &["claude", "codex"]);
    write(
        &sb.claude(),
        r#"{"zeta": 1, "statusLine": {"type": "command", "command": "ccstatusline", "padding": 0}, "alpha": [1, 2]}"#,
    );
    write(
        &sb.codex(),
        r#"{"description": "mine", "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}], "PostModelRequest": [{"hooks": [{"type": "command", "command": "other"}]}]}}"#,
    );
    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("ccstatusline"), "{out}");

    let text = fs::read_to_string(sb.claude()).unwrap();
    let keys: Vec<_> = ["zeta", "statusLine", "alpha"]
        .iter()
        .map(|k| text.find(&format!("\"{k}\"")).unwrap())
        .collect();
    assert!(keys.is_sorted(), "{text}");
    assert_eq!(
        read_json(&sb.claude())["statusLine"],
        json!({"type": "command", "command": "hq ingest claude -- ccstatusline", "padding": 0, "refreshInterval": 30})
    );

    let codex = read_json(&sb.codex());
    assert_eq!(codex["description"], "mine");
    assert_eq!(codex["hooks"]["Stop"][0]["hooks"][0]["command"], "say done");
    let post: Vec<_> = codex["hooks"]["PostModelRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert_eq!(post, ["other", "hq ingest codex"]);
}

#[test]
fn updates_what_an_older_hq_installed() {
    let sb = Sandbox::new("update", &["claude", "codex", "pi"]);
    write(
        &sb.claude(),
        r#"{"statusLine": {"type": "command", "command": "hq claude statusline", "padding": 2}}"#,
    );
    write(
        &sb.codex(),
        r#"{"hooks": {"PostModelRequest": [{"hooks": [{"type": "command", "command": "hq codex hook"}]}]}}"#,
    );
    write(
        &sb.pi(),
        &setup::PI_EXTENSION.replace("TICK_MS = 30_000", "TICK_MS = 5_000"),
    );

    let (code, out) = sb.hq(&["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out.matches("outdated").count(), 3, "{out}");

    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let status = &read_json(&sb.claude())["statusLine"];
    assert_eq!(status["command"], "hq ingest claude");
    assert_eq!(status["padding"], 2);
    assert_eq!(
        read_json(&sb.codex())["hooks"]["PostModelRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fs::read_to_string(sb.pi()).unwrap(), setup::PI_EXTENSION);

    let (code, out) = sb.hq(&["doctor"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.matches("current").count(), 3, "{out}");
}

#[test]
fn broken_config_is_left_alone_and_reported() {
    let sb = Sandbox::new("broken", &["claude", "pi"]);
    write(&sb.claude(), "{ not json");
    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("settings.json"), "{out}");
    assert_eq!(fs::read_to_string(sb.claude()).unwrap(), "{ not json");
    assert!(sb.pi().exists());
}

#[test]
fn writes_through_a_symlinked_config() {
    let sb = Sandbox::new("symlink", &["claude"]);
    let real = sb.home.join("dotfiles/settings.json");
    write(&real, r#"{"theme": "dark"}"#);
    fs::create_dir_all(sb.claude().parent().unwrap()).unwrap();
    symlink(&real, sb.claude()).unwrap();
    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        fs::symlink_metadata(sb.claude())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        read_json(&real)["statusLine"]["command"],
        "hq ingest claude"
    );
}

#[test]
fn remove_takes_out_only_what_hq_installed() {
    let sb = Sandbox::new("remove", &["claude", "codex", "pi"]);
    write(&sb.claude(), r#"{"theme": "dark"}"#);
    write(
        &sb.codex(),
        r#"{"hooks": {"PostModelRequest": [{"hooks": [{"type": "command", "command": "other"}]}]}}"#,
    );
    sb.hq(&["setup", "--yes"]);
    let (code, out) = sb.hq(&["setup", "--remove", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(read_json(&sb.claude()), json!({"theme": "dark"}));
    assert_eq!(
        read_json(&sb.codex()),
        json!({"hooks": {"PostModelRequest": [{"hooks": [{"type": "command", "command": "other"}]}]}})
    );
    assert!(!sb.pi().exists());

    let own = Sandbox::new("remove-own", &["codex", "claude", "pi"]);
    write(
        &own.claude(),
        r#"{"statusLine": {"type": "command", "command": "ccstatusline"}}"#,
    );
    write(&own.pi(), "export default function () {}\n");
    own.hq(&["setup", "codex", "--yes"]);
    let (code, out) = own.hq(&["setup", "--remove", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !own.codex().exists(),
        "a hooks file hq created and emptied is deleted"
    );
    assert_eq!(
        read_json(&own.claude())["statusLine"]["command"],
        "ccstatusline"
    );
    assert!(own.pi().exists());
}

fn change(agent: Agent, foreign: Option<&str>) -> Change {
    Change {
        agent,
        path: PathBuf::from("/tmp/x"),
        summary: "set it".into(),
        foreign: foreign.map(Into::into),
        action: Action::Delete,
    }
}

fn confirm(answers: &str, yes: bool, interactive: bool) -> Result<Vec<Agent>, String> {
    let changes = vec![
        change(Agent::Claude, Some("ccstatusline")),
        change(Agent::Pi, None),
    ];
    let mut out = Vec::new();
    setup::confirm(
        changes,
        yes,
        interactive,
        &mut Cursor::new(answers),
        &mut out,
    )
    .map(|kept| kept.iter().map(|c| c.agent).collect())
}

#[test]
fn asks_once_for_the_plan_and_again_for_each_replacement() {
    assert_eq!(
        confirm("y\ny\n", false, true).unwrap(),
        [Agent::Claude, Agent::Pi]
    );
    assert_eq!(confirm("y\nn\n", false, true).unwrap(), [Agent::Pi]);
    assert_eq!(confirm("n\n", false, true).unwrap(), []);
    assert_eq!(confirm("\n", false, true).unwrap(), []);
    assert_eq!(
        confirm("", true, false).unwrap(),
        [Agent::Claude, Agent::Pi]
    );
    assert!(confirm("y\ny\n", false, false).is_err());
}

#[test]
fn doctor_reports_missing_setup_and_last_report() {
    let sb = Sandbox::new("doctor", &["pi", "codex"]);
    let (code, out) = sb.hq(&["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("hq setup"), "{out}");
    assert!(!out.contains("claude"), "{out}");

    sb.hq(&["setup", "--yes"]);
    let turn = include_str!("fixtures/pi-turn.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hq"))
        .args(["ingest", "pi"])
        .env_clear()
        .env("HOME", &sb.home)
        .env("HC_TERMINAL_ID", "t4")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), turn.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());

    let (code, out) = sb.hq(&["doctor"]);
    assert_eq!(code, 0, "{out}");
    let pi = out.lines().find(|l| l.starts_with("pi")).unwrap();
    assert!(
        pi.contains("current") && pi.contains("last report"),
        "{out}"
    );
    let codex = out.lines().find(|l| l.starts_with("codex")).unwrap();
    assert!(codex.contains("no report yet"), "{out}");
}

#[test]
fn agents_get_the_binary_path_while_hq_is_off_path() {
    let sb = Sandbox::new("offpath", &["claude", "codex", "pi"]);
    fs::remove_file(sb.bin.join("hq")).unwrap();
    let exe = env!("CARGO_BIN_EXE_hq");
    let (code, out) = sb.hq(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        read_json(&sb.claude())["statusLine"]["command"],
        format!("{exe} ingest claude")
    );
    assert_eq!(
        read_json(&sb.codex())["hooks"]["PostModelRequest"][0]["hooks"][0]["command"],
        format!("{exe} ingest codex")
    );
    assert!(
        fs::read_to_string(sb.pi())
            .unwrap()
            .contains(&format!("const HQ = \"{exe}\";"))
    );
    let (code, out) = sb.hq(&["doctor"]);
    assert_eq!(code, 0, "{out}");

    // Once `hq` is on PATH the path entries are stale, and setup moves them to the name.
    symlink(exe, sb.bin.join("hq")).unwrap();
    let (code, out) = sb.hq(&["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out.matches("outdated").count(), 3, "{out}");
    sb.hq(&["setup", "--yes"]);
    assert_eq!(
        read_json(&sb.claude())["statusLine"]["command"],
        "hq ingest claude"
    );
    assert_eq!(fs::read_to_string(sb.pi()).unwrap(), setup::PI_EXTENSION);
    assert_eq!(
        read_json(&sb.codex())["hooks"]["PostModelRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn an_existing_status_line_is_wrapped_and_comes_back_on_remove() {
    for (name, theirs, ours) in [
        (
            "wrap-plain",
            "npx ccstatusline@latest",
            "hq ingest claude -- npx ccstatusline@latest",
        ),
        (
            "wrap-shell",
            "~/bin/line.sh | sed 's/a/b/'",
            r"hq ingest claude -- sh -c '~/bin/line.sh | sed '\''s/a/b/'\'''",
        ),
    ] {
        let sb = Sandbox::new(name, &["claude"]);
        let before = json!({"statusLine": {"type": "command", "command": theirs, "padding": 1}});
        write(&sb.claude(), &before.to_string());

        let (code, out) = sb.hq(&["setup", "--yes"]);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("wrap statusLine"), "{out}");
        assert_eq!(read_json(&sb.claude())["statusLine"]["command"], ours);
        let (code, out) = sb.hq(&["setup", "--yes"]);
        assert!(code == 0 && out.contains("nothing to do"), "{out}");
        let (code, out) = sb.hq(&["doctor"]);
        assert_eq!(code, 0, "{out}");

        let (code, out) = sb.hq(&["setup", "--remove", "--yes"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(read_json(&sb.claude()), before);
    }
}
