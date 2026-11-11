# herdr-queue

`hq` collects what coding agents know about their prompt cache and usage
windows, and keeps one snapshot per [herdr](https://herdr.dev) terminal so
that herdr mods and other tools can read a single shape.

```
hq ingest claude [-- command...]
                   record Claude Code's status line JSON from stdin, then run command on it
hq list [--json]   show every agent session's cache and usage state
```

Snapshots are written only inside herdr, where `HC_TERMINAL_ID` names the
terminal: `$XDG_STATE_HOME/herdr-queue/sessions/<terminal>.json`. Cache
misses that Claude Code reports are appended to `events.jsonl` beside them.

## Agents

| Agent | Cache expiry | Usage windows |
| --- | --- | --- |
| Claude Code | exact, from the status line input | from the status line input |

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
