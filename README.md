# herdr-queue

`hq` collects what coding agents know about their prompt cache and usage
windows, and keeps one snapshot per [herdr](https://herdr.dev) terminal so
that herdr mods and other tools can read a single shape.

```
hq ingest claude [-- command...]
                   record Claude Code's status line JSON from stdin, then run command on it
hq ingest codex    record Codex's PostModelRequest hook JSON from stdin
hq ingest pi       record the pi extension's turn JSON from stdin, print the cache expiry
hq list [--json]   show every agent session's cache and usage state
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

### Claude Code

`~/.claude/settings.json`:

```json
{
  "statusLine": { "type": "command", "command": "hq ingest claude -- ccstatusline", "refreshInterval": 30 }
}
```

`hq` draws no status line of its own. Claude Code runs a single status
line command, so the one that draws goes after `--` and gets the same
input; leave it out to record only.

### Codex

`~/.codex/config.toml`, then approve the hook with `/hooks`:

```toml
[[hooks.PostModelRequest]]
[[hooks.PostModelRequest.hooks]]
type = "command"
command = "hq ingest codex"
```

### pi

Load the extension shipped with the package:

```sh
ln -s "$(dirname "$(readlink -f "$(command -v hq)")")/../share/hq/pi/hq.ts" ~/.pi/agent/extensions/hq.ts
```

It adds the cache countdown to the footer's extension statuses.
