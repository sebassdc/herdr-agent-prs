# herdr-agent-prs

Herdr plugin (`sebassdc.agent-prs`): a strip docked to an agent pane that lists the pull requests **that agent** opened or worked on, across any repos, with live state (draft/open/merged/closed, CI, review, mergeability, diff size). It does not depend on the pane's working directory.

## How it works

1. `herdr agent get <pane>` gives the agent's session id, which is used to find its transcript (Claude `~/.claude/projects/*/<id>.jsonl`, Codex `~/.codex/sessions/**/*<id>*.jsonl`). If no transcript is found, it falls back to `herdr pane read` scrollback.
2. PRs are attributed to the agent only when it acted on them:
   - it ran `gh pr create|merge|edit|ready|comment|close|reopen` (the command itself, parsed per shell segment; heredoc bodies and file contents don't count) or a PR tool such as a GitHub MCP `create_pull_request`;
   - or it ran `git push` and the output shows the pushed GitHub branch. That branch is then resolved to its PRs with one batched `gh` query, so PRs opened in an earlier session still count.
   PR links in the chat are "mentions", hidden unless `show_mentioned = true`. Links in tool output (listings, scans, logs) are ignored.
3. One batched `gh api graphql` call fetches the state of every PR. It refetches when the transcript changes (2 s mtime poll), when the 60 s TTL expires (merged/closed PRs are skipped), or on `r`.

## Requirements

- Herdr >= 0.9, macOS or Linux.
- `gh` installed and logged in (`gh auth login`), authorized for any org whose private repos you want resolved.
- Agents running on the same machine as the Herdr server (transcripts are read from local disk: `~/.claude/projects`, `~/.codex/sessions`).
- Clipboard/opener: `pbcopy`/`open` on macOS, `xclip`/`xdg-open` on Linux.
- Rust toolchain only when no ready-built binary exists for the platform (see Install).

## Install

```sh
herdr plugin install sebassdc/herdr-agent-prs
```

The build hook (`scripts/build.sh`) downloads the release binary for the platform (macOS arm64/x86_64, Linux x86_64/arm64), verifies it against `SHA256SUMS`, and test-runs it. If any step fails, it builds from source with `cargo`. Releases come from `.github/workflows/release.yml` when a `v<version>` tag is pushed.

Local development:

```sh
AGENT_PRS_FROM_SOURCE=1 bash scripts/build.sh   # always rebuild
herdr plugin link ~/dev/herdr-agent-prs
```

Bind the toggle in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+ctrl+p"
type = "plugin_action"
command = "sebassdc.agent-prs.toggle"
description = "toggle agent PR strip"
```

## Keys (inside the strip)

`j/k` move · `o`/Enter open in browser · `y` copy URL · `x` not this agent's PR · `p` this agent's PR · `a` show all (merged, mentions, marked not-mine) · `m` show/hide merged · `r` refresh · `q` close

`x` and `p` toggle, and are saved per agent session. They are also the ground truth for improving detection (see Telemetry).

Running the toggle with the strip focused also closes it.

## Config

`$(herdr plugin config-dir sebassdc.agent-prs)/config.toml`:

```toml
position = "top"       # top | bottom | left | right
max_rows = 8
width = 72             # left/right placement
hide_merged = true
show_mentioned = false  # show dimmed PRs only mentioned in chat
cache_ttl_secs = 60

[telemetry]
enabled = true          # local event log, never sent anywhere
store_excerpts = false  # reserved for Jev
```

## CLI

- `herdr-agent-prs toggle [--pane ID] [--position P]`
- `herdr-agent-prs scan <pane>`: prints detected PRs and their state (debugging).
- `herdr-agent-prs stats`: summarizes the telemetry log.

## Telemetry

Local only, in `~/.local/state/herdr-agent-prs/`:

- `events.jsonl`: one JSON object per line with `ts`, `v` (plugin version) and `event`. Strip events also carry `agent`, `kind` and `session`.
  - `strip_open` / `strip_close`: `position`, `secs`.
  - `scan`: logged when counts change. `source` (transcript/scrollback) and PR counts by rule: `action`, `push`, `chat`.
  - `gh`: `kind` (status/branches), `n`, `ms`, `ok`, `error`.
  - `label`: `pr`, `label` (mine/not_mine/cleared), `reason` (the rule that attributed it), `state`.
  - `action`: `pr`, `action` (open/copy), `reason`.
  - `jev` (reserved): `pr`, `excerpt_sha256`, `excerpt_chars`, `probability`, `threshold`, `decision`, `ms`, `cost_usd`.
- `labels.json`: your `x`/`p` labels per agent session.

`stats` compares the latest label per PR with the rule that fired. It reports false positives (the rule said owned, you said not-mine) and misses (a chat mention you marked mine). These are the cases to tune the rules and Jev against.

## Limits

- Herdr clamps split ratios at 0.1, so a top/bottom strip is at least 10% of the pane height.
- Transcript support covers Claude and Codex. Other agents use scrollback only.
- Jev judging of mention-only PRs is not implemented yet (`[jev]` config and the `jev` event are reserved).

## License

MIT, see [LICENSE](LICENSE). Transcript lookup is adapted from [ChmaraX/herdr-nvim](https://github.com/ChmaraX/herdr-nvim) (MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
