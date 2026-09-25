//! `toggle` action: open or close the PR strip for the focused agent pane.
//!
//! Herdr splits only `right`/`down`, so top/left placements split then swap.
//! Open strips are tracked in `<config dir>/state.json` as
//! `{ "<agent pane>": "<strip pane>" }` so the toggle can find them again and
//! the strip can learn its own pane id.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::{self, Config, Position};
use crate::herdr::{self, PLUGIN_ID};

pub type Strips = BTreeMap<String, String>;

fn state_path() -> Option<PathBuf> {
    config::dir().map(|d| d.join("state.json"))
}

pub fn load_strips() -> Strips {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_strips(s: &Strips) -> Result<()> {
    let p = state_path().context("no plugin config dir")?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

fn focused_pane() -> Option<String> {
    std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|j| serde_json::from_str::<Value>(&j).ok())
        .and_then(|v| v.get("focused_pane_id")?.as_str().map(str::to_owned))
        .or_else(|| std::env::var("HERDR_PANE_ID").ok())
}

pub fn run(target: Option<String>, position: Option<Position>) -> Result<()> {
    let cfg = config::load();
    let focused = target.or_else(focused_pane).context("no focused pane in plugin context")?;

    let mut strips = load_strips();
    strips.retain(|agent, strip| herdr::pane_alive(agent) && herdr::pane_alive(strip));

    // Toggle pressed inside a strip, or on an agent that already has one: close.
    let existing = strips
        .iter()
        .find(|(agent, strip)| **agent == focused || **strip == focused)
        .map(|(a, s)| (a.clone(), s.clone()));
    if let Some((agent, strip)) = existing {
        let _ = herdr::run(&["pane", "close", &strip]);
        strips.remove(&agent);
        return save_strips(&strips);
    }

    let pos = position.unwrap_or(cfg.position);
    let strip = open(&focused, pos, &cfg)?;
    strips.insert(focused, strip);
    save_strips(&strips)
}

fn open(agent: &str, pos: Position, _cfg: &Config) -> Result<String> {
    let dir = if pos.vertical() { "down" } else { "right" };
    let v = herdr::run(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        "strip",
        "--placement",
        "split",
        "--target-pane",
        agent,
        "--direction",
        dir,
        "--env",
        &format!("AGENT_PRS_TARGET={agent}"),
        "--env",
        &format!("AGENT_PRS_POSITION={}", pos.as_str()),
        "--no-focus",
    ])?;
    let Some(strip) = v
        .pointer("/result/plugin_pane/pane/pane_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        bail!("plugin pane open returned no pane id");
    };
    if matches!(pos, Position::Top | Position::Left) {
        herdr::run(&["pane", "swap", "--source-pane", &strip, "--target-pane", agent])?;
        // Focus stays with the screen slot on swap; hand it back to the agent.
        let back = if pos == Position::Top { "down" } else { "right" };
        herdr::run(&["pane", "focus", "--direction", back, "--pane", &strip])?;
    }
    Ok(strip)
}
