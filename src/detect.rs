//! Extract GitHub PR references from an agent transcript or pane scrollback.
//!
//! **Owned**: the agent actually acted on the PR.
//! - A shell tool call whose command (split into segments, heredoc bodies
//!   removed) has a segment starting with `gh pr <verb>`. URLs among that
//!   segment's arguments are owned, and so are URLs in that call's output
//!   (e.g. `gh pr create` prints the new PR URL).
//! - A PR tool call (e.g. a GitHub MCP `create_pull_request`): URLs in its
//!   input and output are owned.
//!
//! **Mentioned**: a PR URL in the conversation text itself (user or
//! assistant message). URLs inside other tool inputs/outputs (listings,
//! scans, logs, file contents) are ignored: reading about a PR is not
//! working on it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

static PR_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/pull/(\d+)").unwrap()
});

/// Tool names that run shell commands (Claude `Bash`, Codex `exec`/`shell`).
const SHELL_TOOLS: &[&str] = &["Bash", "exec", "exec_command", "shell", "container.exec", "local_shell"];

/// Non-shell tools that create or change PRs (GitHub MCP servers and similar).
static PR_TOOL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(create|update|merge|edit)_?(pull_?request|pr)\b").unwrap()
});

static GH_PR_SEGMENT: LazyLock<Regex> = LazyLock::new(|| {
    // Segment start: optional prompt / subshell / env assignments, then gh pr <verb>.
    Regex::new(r"^(?:\$\s+|❯\s+|\(\s*|[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*gh\s+pr\s+(create|merge|edit|ready|comment|close|reopen)\b").unwrap()
});

static SEGMENT_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n|;|&&|\|\||\|").unwrap());

static HEREDOC_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).unwrap());

/// Codex `exec` wraps shell commands in JS: `tools.exec_command({cmd:"..."})`.
static JS_CMD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bcmd\s*:\s*("(?:[^"\\]|\\.)*")"#).unwrap());
static JS_CMD_TEMPLATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bcmd\s*:\s*`([^`]*)`").unwrap());

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}/pull/{}", self.owner, self.repo, self.number)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    Mentioned,
    Owned,
}

/// PR -> strongest signal seen, plus a first-seen order for stable display.
pub type Found = BTreeMap<PrRef, (Signal, usize)>;

/// A branch the agent pushed to GitHub. Resolved to PRs via `gh` later.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Push {
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

pub type Pushes = BTreeSet<Push>;

static GIT_PUSH_SEGMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\$\s+|❯\s+|\(\s*|[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*git\s+(?:-C\s+\S+\s+)?push\b").unwrap()
});
static PUSH_REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^To (?:https://github\.com/|(?:git@)?github\.com:|ssh://git@github\.com/)([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+?)(?:\.git)?/?\s*$").unwrap()
});
/// ` * [new branch]  a -> b`, `   1a2b..3c4d  a -> b`, ` + 1a...2b a -> b (forced update)`.
/// Rejected (`!`) and up-to-date (`=`) lines are skipped.
static PUSH_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^ [ *+-] +(?:\[[^\]]+\]|[0-9a-f]+\.\.\.?[0-9a-f]+) +\S+ -> (\S+)").unwrap()
});
const DEFAULT_BRANCHES: &[&str] = &["main", "master", "develop", "trunk"];

/// Parse `git push` output into the pushed GitHub branches.
pub fn pushes_from_output(text: &str) -> Vec<Push> {
    let mut out = Vec::new();
    let remotes: Vec<(usize, String, String)> = PUSH_REMOTE
        .captures_iter(text)
        .map(|c| (c.get(0).unwrap().start(), c[1].to_owned(), c[2].to_owned()))
        .collect();
    for c in PUSH_REF.captures_iter(text) {
        let at = c.get(0).unwrap().start();
        // The nearest preceding `To <remote>` line owns this ref line.
        let Some((_, owner, repo)) = remotes.iter().rev().find(|(pos, _, _)| *pos < at) else {
            continue;
        };
        let branch = c[1].trim_start_matches("refs/heads/").to_owned();
        if DEFAULT_BRANCHES.contains(&branch.as_str()) || branch.starts_with("refs/") {
            continue;
        }
        out.push(Push { owner: owner.clone(), repo: repo.clone(), branch });
    }
    out
}

fn urls(text: &str) -> impl Iterator<Item = PrRef> + '_ {
    PR_URL.captures_iter(text).filter_map(|c| {
        Some(PrRef {
            owner: c[1].to_owned(),
            repo: c[2].trim_end_matches(".git").to_owned(),
            number: c[3].parse().ok()?,
        })
    })
}

fn add(found: &mut Found, pr: PrRef, sig: Signal) {
    let n = found.len();
    let e = found.entry(pr).or_insert((sig, n));
    if sig > e.0 {
        e.0 = sig;
    }
}

/// Drop heredoc bodies: their lines are data, not commands.
fn strip_heredocs(cmd: &str) -> String {
    let mut out = String::new();
    let mut end_tag: Option<String> = None;
    for line in cmd.lines() {
        if let Some(tag) = &end_tag {
            if line.trim() == tag {
                end_tag = None;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if let Some(c) = HEREDOC_START.captures(line) {
            end_tag = Some(c[1].to_owned());
        }
    }
    out
}

/// Scan one shell command. Returns (ran a gh pr verb, URLs in those segments).
pub fn gh_pr_segments(cmd: &str) -> (bool, Vec<PrRef>) {
    let mut ran = false;
    let mut owned = Vec::new();
    for seg in SEGMENT_SPLIT.split(&strip_heredocs(cmd)) {
        let seg = seg.trim();
        if GH_PR_SEGMENT.is_match(seg) {
            ran = true;
            owned.extend(urls(seg));
        }
    }
    (ran, owned)
}

fn runs_git_push(cmd: &str) -> bool {
    SEGMENT_SPLIT
        .split(&strip_heredocs(cmd))
        .any(|seg| GIT_PUSH_SEGMENT.is_match(seg.trim()))
}

/// Plain-text source (pane scrollback): a URL is owned when a nearby line is
/// a `gh pr <verb>` command; otherwise a mention.
pub fn from_text(text: &str, found: &mut Found, pushes: &mut Pushes) {
    pushes.extend(pushes_from_output(text));
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        for pr in urls(line) {
            let lo = i.saturating_sub(3);
            let owned = lines[lo..=i].iter().any(|l| gh_pr_segments(l).0);
            add(found, pr, if owned { Signal::Owned } else { Signal::Mentioned });
        }
    }
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The shell command(s) a shell tool call actually ran.
fn shell_commands(v: &Value) -> Vec<String> {
    let raw = v.get("input").or_else(|| v.get("arguments")).or_else(|| v.get("action"));
    let obj = match raw {
        Some(Value::String(r)) => match serde_json::from_str::<Value>(r) {
            Ok(parsed @ Value::Object(_)) => parsed,
            _ => {
                // Codex `exec`: JS source; pull out each cmd:"..." literal.
                let cmds: Vec<String> = JS_CMD
                    .captures_iter(r)
                    .filter_map(|c| serde_json::from_str::<String>(&c[1]).ok())
                    .chain(JS_CMD_TEMPLATE.captures_iter(r).map(|c| c[1].to_owned()))
                    .collect();
                return if cmds.is_empty() { vec![r.clone()] } else { cmds };
            }
        },
        Some(o @ Value::Object(_)) => o.clone(),
        _ => v.clone(),
    };
    for key in ["command", "cmd"] {
        match obj.get(key) {
            Some(Value::String(c)) => return vec![c.clone()],
            Some(Value::Array(parts)) => {
                return vec![parts.iter().map(text_of).collect::<Vec<_>>().join(" ")];
            }
            _ => {}
        }
    }
    Vec::new()
}

#[derive(Default)]
struct Ctx {
    /// call id -> the call acts on PRs (its output URLs are owned).
    owning_calls: HashMap<String, bool>,
    /// call ids of shell calls that ran `git push`.
    push_calls: HashSet<String>,
    owned: Vec<PrRef>,
    pushes: Vec<Push>,
    mentioned: Vec<PrRef>,
}

impl Ctx {
    fn on_call(&mut self, id: Option<&str>, v: &Value) {
        let name = s(v, "name").unwrap_or("");
        let shell = s(v, "type") == Some("local_shell_call") || SHELL_TOOLS.contains(&name);
        let owning = if shell {
            let mut ran = false;
            for cmd in shell_commands(v) {
                let (r, prs) = gh_pr_segments(&cmd);
                ran |= r;
                self.owned.extend(prs);
                if runs_git_push(&cmd) {
                    if let Some(id) = id {
                        self.push_calls.insert(id.to_owned());
                    }
                }
            }
            ran
        } else if PR_TOOL.is_match(name) {
            self.owned.extend(urls(&v.to_string()));
            true
        } else {
            false
        };
        if let Some(id) = id {
            self.owning_calls.insert(id.to_owned(), owning);
        }
    }

    fn on_output(&mut self, id: Option<&str>, out: &Value) {
        if id.and_then(|i| self.owning_calls.get(i)).copied().unwrap_or(false) {
            self.owned.extend(urls(&text_of(out)));
        }
        if id.is_some_and(|i| self.push_calls.contains(i)) {
            let text = match out {
                // Codex outputs are arrays of {type, text}.
                Value::Array(items) => items.iter().map(|i| s(i, "text").map(str::to_owned).unwrap_or_else(|| text_of(i))).collect::<Vec<_>>().join("\n"),
                other => text_of(other),
            };
            // Codex nests tool output as JSON strings, so newlines may be escaped.
            self.pushes.extend(pushes_from_output(&text.replace("\\n", "\n")));
        }
    }

    /// Codex `CommandExecution` item: command and output in one record.
    fn on_command_execution(&mut self, item: &Value) {
        let cmd = match item.get("command") {
            Some(Value::Array(parts)) => parts.last().map(text_of).unwrap_or_default(),
            Some(other) => text_of(other),
            None => return,
        };
        let raw = s(item, "aggregated_output").or_else(|| s(item, "stdout")).unwrap_or("");
        let output = serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_owned());
        let (ran, prs) = gh_pr_segments(&cmd);
        self.owned.extend(prs);
        if ran {
            self.owned.extend(urls(&output));
        }
        if runs_git_push(&cmd) {
            self.pushes.extend(pushes_from_output(&output));
        }
    }

    fn on_chat_text(&mut self, text: &str) {
        self.mentioned.extend(urls(text));
    }
}

/// JSONL transcript (Claude or Codex).
pub fn from_jsonl(text: &str, found: &mut Found, pushes: &mut Pushes) {
    let mut ctx = Ctx::default();
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        record(&rec, &mut ctx);
        for pr in ctx.owned.drain(..) {
            add(found, pr, Signal::Owned);
        }
        for pr in ctx.mentioned.drain(..) {
            add(found, pr, Signal::Mentioned);
        }
        pushes.extend(ctx.pushes.drain(..));
    }
}

fn record(rec: &Value, ctx: &mut Ctx) {
    // Claude: {"type":"user"|"assistant","message":{"role","content"}}
    if let Some(msg) = rec.get("message") {
        let role = s(msg, "role").unwrap_or("");
        match msg.get("content") {
            Some(Value::String(t)) if role == "user" || role == "assistant" => ctx.on_chat_text(t),
            Some(Value::Array(items)) => {
                for it in items {
                    match s(it, "type") {
                        Some("text") if role == "user" || role == "assistant" => {
                            ctx.on_chat_text(s(it, "text").unwrap_or(""))
                        }
                        Some("tool_use") => ctx.on_call(s(it, "id"), it),
                        Some("tool_result") => ctx.on_output(s(it, "tool_use_id"), it.get("content").unwrap_or(&Value::Null)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        return;
    }
    // Codex: {"type":"response_item","payload":{...}} and event_msg items.
    let Some(p) = rec.get("payload") else { return };
    match s(p, "type") {
        Some("message") if matches!(s(p, "role"), Some("user" | "assistant")) => {
            if let Some(Value::Array(items)) = p.get("content") {
                for it in items {
                    if matches!(s(it, "type"), Some("input_text" | "output_text" | "text")) {
                        ctx.on_chat_text(s(it, "text").unwrap_or(""));
                    }
                }
            }
        }
        Some("function_call" | "custom_tool_call" | "local_shell_call") => ctx.on_call(s(p, "call_id"), p),
        Some("item_completed") => {
            if let Some(item) = p.get("item").filter(|i| s(i, "type") == Some("CommandExecution")) {
                ctx.on_command_execution(item);
            }
        }
        Some("function_call_output" | "custom_tool_call_output" | "local_shell_call_output") => {
            ctx.on_output(s(p, "call_id"), p.get("output").unwrap_or(&Value::Null))
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(found: &Found) -> Vec<(u64, Signal)> {
        found.iter().map(|(k, v)| (k.number, v.0)).collect()
    }

    fn scan(t: &str) -> Vec<(u64, Signal)> {
        let mut f = Found::new();
        from_jsonl(t, &mut f, &mut Pushes::new());
        only(&f)
    }

    #[test]
    fn git_push_output_yields_branches() {
        let out = "remote: Create a pull request for 'feat/x':\nTo github.com:acme/drip.git\n * [new branch]      feat/x -> feat/x\n   1a2b3c..4d5e6f  HEAD -> fix/y\n ! [rejected]        other -> other (fetch first)\n   aaa111..bbb222  main -> main\n";
        let got: Vec<String> = pushes_from_output(out).into_iter().map(|p| format!("{}/{}:{}", p.owner, p.repo, p.branch)).collect();
        assert_eq!(got, vec!["acme/drip:feat/x", "acme/drip:fix/y"]);
    }

    #[test]
    fn claude_git_push_call_collects_push() {
        let t = concat!(
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","id":"p1","name":"Bash","input":{"command":"git push -u origin HEAD"}}]}}"#,
            "\n",
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"p1","content":"To github.com:o/r.git\n * [new branch]      HEAD -> feat/z\n"}]}}"#,
        );
        let mut f = Found::new();
        let mut p = Pushes::new();
        from_jsonl(t, &mut f, &mut p);
        assert_eq!(p.into_iter().map(|p| p.branch).collect::<Vec<_>>(), vec!["feat/z"]);
    }

    #[test]
    fn claude_gh_pr_create_is_owned_and_chat_is_mentioned() {
        let t = concat!(
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git push && gh pr create --fill"}}]}}"#,
            "\n",
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"https://github.com/acme/ask/pull/42\n"}]}}"#,
            "\n",
            r#"{"message":{"role":"assistant","content":[{"type":"text","text":"see https://github.com/acme/flow/pull/7"}]}}"#,
        );
        assert_eq!(scan(t), vec![(42, Signal::Owned), (7, Signal::Mentioned)]);
    }

    #[test]
    fn tool_output_urls_are_ignored() {
        let t = concat!(
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"gh pr list -R o/r"}}]}}"#,
            "\n",
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"https://github.com/o/r/pull/3"}]}}"#,
        );
        assert_eq!(scan(t), vec![]);
    }

    #[test]
    fn heredoc_and_embedded_text_are_not_commands() {
        let cmd = "cat > f.rs <<'EOF'\ngh pr merge https://github.com/o/r/pull/9\nEOF\npython3 - <<'PY'\nx = '\"cmd\":\"gh pr merge https://github.com/o/r/pull/8'\nPY";
        let rec = serde_json::json!({"message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":cmd}}]}});
        assert_eq!(scan(&rec.to_string()), vec![]);
    }

    #[test]
    fn file_write_containing_command_is_ignored() {
        let t = r#"{"message":{"content":[{"type":"tool_use","id":"w1","name":"Write","input":{"content":"gh pr create https://github.com/o/r/pull/5"}}]}}"#;
        assert_eq!(scan(t), vec![]);
    }

    #[test]
    fn codex_exec_js_and_function_call() {
        let t = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1","arguments":"{\"cmd\":\"gh pr merge https://github.com/o/r/pull/9 --squash\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c2","input":"const r = await tools.exec_command({cmd:\"gh pr create -R o/s --title \\\"x\\\"\"}); text(r)"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c2","output":[{"type":"input_text","text":"https://github.com/o/s/pull/10"}]}}"#,
        );
        assert_eq!(scan(t), vec![(9, Signal::Owned), (10, Signal::Owned)]);
    }

    #[test]
    fn codex_js_template_literal() {
        let t = concat!(
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c3","input":"await tools.exec_command({cmd:`gh pr create -R o/t --fill`})"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c3","output":"https://github.com/o/t/pull/12"}}"#,
        );
        assert_eq!(scan(t), vec![(12, Signal::Owned)]);
    }

    #[test]
    fn codex_command_execution_push() {
        let rec = serde_json::json!({"type":"event_msg","payload":{"type":"item_completed","item":{
            "type":"CommandExecution","command":["/bin/zsh","-lc","git push -u origin feat/q"],
            "aggregated_output":"\"To github.com:o/r.git\\n * [new branch]      feat/q -> feat/q\\n\""}}});
        let mut f = Found::new();
        let mut p = Pushes::new();
        from_jsonl(&rec.to_string(), &mut f, &mut p);
        assert_eq!(p.into_iter().map(|p| p.branch).collect::<Vec<_>>(), vec!["feat/q"]);
    }

    #[test]
    fn mcp_pr_tool_is_owned() {
        let t = concat!(
            r#"{"message":{"role":"assistant","content":[{"type":"tool_use","id":"m1","name":"mcp__github__create_pull_request","input":{"owner":"o","repo":"r"}}]}}"#,
            "\n",
            r#"{"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"m1","content":"{\"html_url\":\"https://github.com/o/r/pull/11\"}"}]}}"#,
        );
        assert_eq!(scan(t), vec![(11, Signal::Owned)]);
    }

    #[test]
    fn scrollback_window() {
        let mut f = Found::new();
        from_text("$ gh pr create\nhttps://github.com/o/r/pull/1\n\n\n\n\nhttps://github.com/o/r/pull/2", &mut f, &mut Pushes::new());
        assert_eq!(only(&f), vec![(1, Signal::Owned), (2, Signal::Mentioned)]);
    }
}
