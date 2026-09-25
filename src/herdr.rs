//! Thin wrapper over the `herdr` CLI. Every call returns parsed JSON.

use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub const PLUGIN_ID: &str = "sebassdc.agent-prs";

fn bin() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned())
}

pub fn run(args: &[&str]) -> Result<Value> {
    let out = Command::new(bin())
        .args(args)
        .output()
        .with_context(|| format!("failed to run herdr {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "herdr {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("herdr {} returned non-JSON", args[0]))
}

/// Agent occupying a pane, as reported by `herdr agent get`.
#[derive(Debug, Clone, Default)]
pub struct AgentInfo {
    pub name: Option<String>,
    pub agent: Option<String>,
    pub cwd: Option<String>,
    /// `agent_session.kind` ("id" or "path") and `agent_session.value`.
    pub session_kind: Option<String>,
    pub session_value: Option<String>,
}

pub fn agent_info(pane: &str) -> Option<AgentInfo> {
    let v = run(&["agent", "get", pane]).ok()?;
    let a = v.pointer("/result/agent").unwrap_or(&v);
    let s = |p: &str| a.pointer(p).and_then(Value::as_str).map(str::to_owned);
    Some(AgentInfo {
        name: s("/name"),
        agent: s("/agent"),
        cwd: s("/foreground_cwd").or_else(|| s("/cwd")),
        session_kind: s("/agent_session/kind"),
        session_value: s("/agent_session/value"),
    })
}

pub fn pane_alive(pane: &str) -> bool {
    run(&["pane", "get", pane]).is_ok()
}

/// Recent pane output, soft wraps joined.
pub fn read_pane(pane: &str, lines: u32) -> Result<String> {
    let out = Command::new(bin())
        .args([
            "pane",
            "read",
            pane,
            "--source",
            "recent-unwrapped",
            "--lines",
            &lines.to_string(),
        ])
        .output()?;
    if !out.status.success() {
        bail!("herdr pane read failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn config_dir() -> Option<std::path::PathBuf> {
    let out = Command::new(bin())
        .args(["plugin", "config-dir", PLUGIN_ID])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    // Accept either a bare path or a JSON envelope.
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        for p in ["/result/config_dir", "/result/path", "/config_dir", "/path"] {
            if let Some(s) = v.pointer(p).and_then(Value::as_str) {
                return Some(s.into());
            }
        }
        return None;
    }
    (!text.is_empty()).then(|| text.into())
}
