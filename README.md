# herdr-queue

`hq` collects what coding agents know about their prompt cache and usage
windows, and keeps one snapshot per [herdr](https://herdr.dev) terminal so
that herdr mods and other tools can read a single shape.

```
hq setup [claude|codex|pi] [--remove] [--yes]
                   connect hq to the agents on PATH, or to the one named
hq doctor          check that each agent is connected and reporting
hq list [--json]   show every agent session's cache and usage state
hq usage [--refresh] [--json]
                   show each account's usage against its rate limits; --refresh asks
                   claude and codex first

hq ingest claude [-- command...]
                   record Claude Code's status line JSON from stdin, then run command on it
hq ingest codex    record Codex's PostModelRequest hook JSON from stdin
hq ingest pi       record the pi extension's turn JSON from stdin, print the cache expiry
```

Snapshots are written only inside herdr, where `HC_TERMINAL_ID` names the
terminal: `$XDG_STATE_HOME/herdr-queue/sessions/<terminal>.json`. Cache
misses that Claude Code reports are appended to `events.jsonl` beside them.

## Agents

| Agent | Cache expiry | Usage windows |
| --- | --- | --- |
| Claude Code | exact, from the status line input | from the status line input |
| Codex | estimated: last request + 5 minutes | from the rollout file |
| pi | estimated: last cached request + the model's lifetime, 5 minutes if it declares none | none |

An estimated expiry is the earliest the cache can lapse, and is marked
`"certainty": "estimated"` in the snapshot.

## Account usage

Rate limits belong to an account, not to a terminal, and a session only
learns them when it makes a request. `hq usage --refresh` asks each agent's
own program instead, so `hq` never reads or renews a token:

| Agent | Asked with | Gives |
| --- | --- | --- |
| Claude Code | `claude -p`, control request `get_usage` | session and weekly windows, each model's weekly window, and the plan |
| Codex | `codex app-server`, `account/rateLimits/read` | its windows and the plan |

Answers are kept in `$XDG_STATE_HOME/herdr-queue/accounts/<agent>.json`. An
agent is asked at most once every five minutes, and less often after each
failure, because both programs answer from an endpoint that throttles hard.
A caller that wants fresh numbers runs `hq usage --refresh` on a timer; ten
minutes is plenty.

`hq usage` shows the answer while it is recent. When asking fails, or the
answer is more than half an hour old and a terminal has reported since, it
shows what that terminal's session last saw instead and names the source as
`terminal`. Those reports have no per-model windows.

Neither question starts a turn or spends quota. Claude Code runs in safe
mode without session persistence, so it loads no hooks or MCP servers and
leaves no session file behind.

## Setup

`hq setup` finds `claude`, `codex` and `pi` on `PATH`, shows the file it
would change for each, and writes only after you agree:

| Agent | File | Change |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | `statusLine` runs `hq ingest claude`, wrapped around the status line command already there |
| Codex | `~/.codex/hooks.json` | a `PostModelRequest` hook runs `hq ingest codex`; approve it with `/hooks` |
| pi | `~/.pi/agent/extensions/hq.ts` | an extension that reports each turn and shows the countdown in the footer |

`hq` draws no status line of its own. Claude Code runs a single status
line command, so setup turns an existing `ccstatusline` into
`hq ingest claude -- ccstatusline`: `hq` records the input and hands it
on, and `--remove` puts the original command back. It asks a second time
before replacing a pi extension it did not install.

Run it again after upgrading `hq` to bring these entries up to date, and
`hq setup --remove` to take out only what it installed.
`hq doctor` shows whether each agent is connected, current, and reporting.

With Nix, the extension can be linked instead of written; `hq doctor`
treats it the same:

```nix
home.file.".pi/agent/extensions/hq.ts".source = hq.integrations.pi;
```

The agents call `hq` by name. When `hq` is not on `PATH`, setup writes
the full path of the binary it was run from instead; run it again after
that path changes or `hq` lands on `PATH`.
