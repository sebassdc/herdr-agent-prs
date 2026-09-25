//! `config.toml` in the plugin config dir (`herdr plugin config-dir sebassdc.agent-prs`).

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl Position {
    pub fn vertical(self) -> bool {
        matches!(self, Position::Top | Position::Bottom)
    }
    /// Direction that moves the shared border toward the strip (shrinks it).
    pub fn shrink_dir(self) -> &'static str {
        match self {
            Position::Top => "up",
            Position::Bottom => "down",
            Position::Left => "left",
            Position::Right => "right",
        }
    }
    pub fn grow_dir(self) -> &'static str {
        match self {
            Position::Top => "down",
            Position::Bottom => "up",
            Position::Left => "right",
            Position::Right => "left",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        toml::Value::String(s.to_owned()).try_into().ok()
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Position::Top => "top",
            Position::Bottom => "bottom",
            Position::Left => "left",
            Position::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub position: Position,
    /// Max PR rows before the strip scrolls (top/bottom placement).
    pub max_rows: u16,
    /// Strip width in columns (left/right placement).
    pub width: u16,
    pub hide_merged: bool,
    pub show_mentioned: bool,
    pub cache_ttl_secs: u64,
    pub jev: Jev,
    pub telemetry: Telemetry,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Telemetry {
    /// Append events to ~/.local/state/herdr-agent-prs/events.jsonl (local only).
    pub enabled: bool,
    /// Reserved for Jev: keep transcript excerpts sent to Jev (default: hash only).
    pub store_excerpts: bool,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self { enabled: true, store_excerpts: false }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Jev {
    /// Reserved: judge mention-only PRs with TypeSafe Jev. Not implemented in v1.
    pub enabled: bool,
    pub threshold: Option<f64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            position: Position::Top,
            max_rows: 8,
            width: 72,
            hide_merged: true,
            show_mentioned: false,
            cache_ttl_secs: 60,
            jev: Jev::default(),
            telemetry: Telemetry::default(),
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    std::env::var_os("AGENT_PRS_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(crate::herdr::config_dir)
}

pub fn load() -> Config {
    dir()
        .and_then(|d| std::fs::read_to_string(d.join("config.toml")).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}
