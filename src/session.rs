//! Locate an agent's on-disk transcript from Herdr's `agent_session` metadata.
//! Approach adapted from ChmaraX/herdr-nvim `sessions.rs` (MIT).

use std::path::{Path, PathBuf};

use crate::herdr::AgentInfo;

/// Main transcript plus, for Claude, the transcripts of subagents and
/// Workflow agents it spawned (`<session dir>/subagents/**`, `workflows/**`).
/// PRs opened by a subagent belong to the agent that delegated the work.
pub fn transcript_paths(info: &AgentInfo) -> Vec<PathBuf> {
    let Some(main) = transcript_path(info) else { return Vec::new() };
    let mut paths = vec![main.clone()];
    if info.agent.as_deref() == Some("claude") {
        let dir = main.with_extension("");
        collect_jsonl(&dir.join("subagents"), &mut paths);
        collect_jsonl(&dir.join("workflows"), &mut paths);
    }
    paths
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().is_some_and(|e| e == "jsonl") {
            out.push(p);
        }
    }
}

pub fn transcript_path(info: &AgentInfo) -> Option<PathBuf> {
    let value = info.session_value.as_deref()?;
    if info.session_kind.as_deref() == Some("path") {
        return Some(PathBuf::from(value));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    match info.agent.as_deref()? {
        "claude" => claude(&home, value, info.cwd.as_deref()),
        "codex" => codex(&home, value),
        _ => None,
    }
}

/// `~/.claude/projects/<cwd-slug>/<id>.jsonl`; slug encoding drifts across
/// versions, so fall back to scanning every project dir.
fn claude(home: &Path, id: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let projects = home.join(".claude/projects");
    let file = format!("{id}.jsonl");
    if let Some(cwd) = cwd {
        let slug: String = cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let direct = projects.join(slug).join(&file);
        if direct.is_file() {
            return Some(direct);
        }
    }
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

/// `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`. Walk newest first.
fn codex(home: &Path, id: &str) -> Option<PathBuf> {
    fn sorted_desc(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        v.sort();
        v.reverse();
        v
    }
    let root = home.join(".codex/sessions");
    for year in sorted_desc(&root) {
        for month in sorted_desc(&year) {
            for day in sorted_desc(&month) {
                for f in sorted_desc(&day) {
                    let name = f.file_name()?.to_string_lossy().into_owned();
                    if name.ends_with(".jsonl") && name.contains(id) {
                        return Some(f);
                    }
                }
            }
        }
    }
    None
}
