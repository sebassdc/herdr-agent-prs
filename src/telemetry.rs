//! Local-only telemetry and labels. Nothing leaves the machine.
//!
//! - `events.jsonl`: one JSON object per line, `{ts, v, event, ...fields}`.
//! - `labels.json`: user corrections, `{ "<agent session>": { "<pr url>": "mine" | "not_mine" } }`.
//!
//! Both live in `$XDG_STATE_HOME/herdr-agent-prs/` (default `~/.local/state/...`).
//! Event schema is documented in README ("Telemetry").

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub fn state_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|d| d.join("herdr-agent-prs"))
}

pub fn events_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join("events.jsonl"))
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Append one event. Failures are swallowed: telemetry must never break the UI.
pub fn log(enabled: bool, event: &str, fields: Value) {
    if !enabled {
        return;
    }
    let Some(path) = events_path() else { return };
    let mut rec = json!({ "ts": now_ms() as u64, "v": env!("CARGO_PKG_VERSION"), "event": event });
    if let (Some(obj), Value::Object(extra)) = (rec.as_object_mut(), fields) {
        obj.extend(extra);
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{rec}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Mine,
    NotMine,
}

impl Label {
    fn as_str(self) -> &'static str {
        match self {
            Label::Mine => "mine",
            Label::NotMine => "not_mine",
        }
    }
}

type LabelFile = BTreeMap<String, BTreeMap<String, String>>;

fn labels_path() -> Option<PathBuf> {
    state_dir().map(|d| d.join("labels.json"))
}

fn read_labels() -> LabelFile {
    labels_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Labels for one agent session: PR url -> label.
pub fn labels_for(session: &str) -> BTreeMap<String, Label> {
    read_labels()
        .remove(session)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(url, l)| {
            let l = match l.as_str() {
                "mine" => Label::Mine,
                "not_mine" => Label::NotMine,
                _ => return None,
            };
            Some((url, l))
        })
        .collect()
}

/// Set or clear (`None`) a label.
pub fn set_label(session: &str, url: &str, label: Option<Label>) {
    let Some(path) = labels_path() else { return };
    let mut all = read_labels();
    let entry = all.entry(session.to_owned()).or_default();
    match label {
        Some(l) => {
            entry.insert(url.to_owned(), l.as_str().to_owned());
        }
        None => {
            entry.remove(url);
        }
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(path, text);
    }
}

/// `herdr-agent-prs stats`: summarize the event log.
pub fn stats() -> anyhow::Result<()> {
    let path = events_path().ok_or_else(|| anyhow::anyhow!("no state dir"))?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let events: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    println!("log: {} ({} events)", path.display(), events.len());
    if events.is_empty() {
        return Ok(());
    }
    let of = |name: &'static str| events.iter().filter(move |e| e["event"] == name);

    let mut by_event: BTreeMap<String, usize> = BTreeMap::new();
    for e in &events {
        *by_event.entry(e["event"].as_str().unwrap_or("?").to_owned()).or_default() += 1;
    }
    println!("\nevents:");
    for (k, n) in &by_event {
        println!("  {k:<14} {n}");
    }

    println!("\ngh calls:");
    for kind in ["status", "branches"] {
        let calls: Vec<&Value> = of("gh").filter(|e| e["kind"] == kind).collect();
        if calls.is_empty() {
            continue;
        }
        let errors = calls.iter().filter(|e| e["ok"] == false).count();
        let ms: f64 = calls.iter().filter_map(|e| e["ms"].as_f64()).sum::<f64>() / calls.len() as f64;
        println!("  {kind:<9} {} calls, {errors} errors, avg {ms:.0} ms", calls.len());
    }

    let jev: Vec<&Value> = of("jev").collect();
    println!("\njev calls: {}", jev.len());
    if !jev.is_empty() {
        let kept = jev.iter().filter(|e| e["decision"] == "keep").count();
        let ms: f64 = jev.iter().filter_map(|e| e["ms"].as_f64()).sum::<f64>() / jev.len() as f64;
        let cost: f64 = jev.iter().filter_map(|e| e["cost_usd"].as_f64()).sum();
        println!("  kept {kept}, hidden {}, avg {ms:.0} ms, cost ${cost:.4}", jev.len() - kept);
    }

    // Labels are ground truth. Only the latest label per (session, PR) counts.
    println!("\nlabels (your corrections, latest per PR):");
    let mut latest: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for e in of("label") {
        let key = (e["session"].as_str().unwrap_or("").to_owned(), e["pr"].as_str().unwrap_or("").to_owned());
        let val = (e["reason"].as_str().unwrap_or("?").to_owned(), e["label"].as_str().unwrap_or("?").to_owned());
        latest.insert(key, val);
    }
    let mut table: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (_, (reason, label)) in latest.into_iter().filter(|(_, (_, l))| l != "cleared") {
        *table.entry((reason, label)).or_default() += 1;
    }
    if table.is_empty() {
        println!("  none yet (x = not this agent's PR, p = it is)");
    }
    for ((reason, label), n) in &table {
        let verdict = match (reason.as_str(), label.as_str()) {
            ("chat", "mine") => "rule missed",
            ("chat", "not_mine") => "agrees",
            (_, "not_mine") => "false positive",
            _ => "agrees",
        };
        println!("  rule={reason:<7} label={label:<9} {n:>4}  ({verdict})");
    }
    Ok(())
}
