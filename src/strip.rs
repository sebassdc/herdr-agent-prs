//! `strip` pane entrypoint: the docked PR list for one agent pane.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Row as TRow, Table, TableState},
};
use serde_json::Value;

use crate::config::{self, Config, Position};
use crate::detect::{self, Found, PrRef, Push, Pushes, Signal};
use crate::github::{self, Ci, Merge, PrStatus, Review, State};
use crate::herdr;
use crate::session;
use crate::telemetry::{self, Label};
use crate::toggle;

const TICK: Duration = Duration::from_millis(250);
const MTIME_EVERY: Duration = Duration::from_secs(2);
const RESOLVE_EVERY: Duration = Duration::from_secs(15);
const SCROLLBACK_EVERY: Duration = Duration::from_secs(10);
const ALIVE_EVERY: Duration = Duration::from_secs(5);

struct Fetched {
    /// Pushed branch -> PRs whose head is that branch.
    branches: Vec<(Push, Vec<PrRef>)>,
    statuses: Vec<(PrRef, Result<PrStatus, String>)>,
}

type FetchResult = Result<Fetched, String>;

/// Resolve pushed branches to PRs, then fetch state for `due` plus any new PRs.
fn fetch_all(due: Vec<PrRef>, branches: Vec<Push>, tel: bool) -> anyhow::Result<Fetched> {
    fn timed<T>(tel: bool, kind: &str, n: usize, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        if n == 0 {
            return f();
        }
        let t = Instant::now();
        let r = f();
        let err = r.as_ref().err().map(|e| e.to_string());
        telemetry::log(tel, "gh", serde_json::json!({
            "kind": kind, "n": n, "ms": t.elapsed().as_millis() as u64, "ok": err.is_none(), "error": err,
        }));
        r
    }
    let resolved = timed(tel, "branches", branches.len(), || github::prs_for_branches(&branches))?;
    let branches: Vec<(Push, Vec<PrRef>)> = branches.into_iter().zip(resolved).collect();
    let mut prs = due;
    for (_, found) in &branches {
        for pr in found {
            if !prs.contains(pr) {
                prs.push(pr.clone());
            }
        }
    }
    let statuses = timed(tel, "status", prs.len(), || github::fetch(&prs))?;
    Ok(Fetched { branches, statuses: prs.into_iter().zip(statuses).collect() })
}

/// Why a PR is attributed to the agent (kept for telemetry/labels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reason {
    Chat,
    Push,
    Action,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Chat => "chat",
            Reason::Push => "push",
            Reason::Action => "action",
        }
    }
}

struct Entry {
    pr: PrRef,
    signal: Signal,
    reason: Reason,
    order: usize,
    status: Option<Result<PrStatus, String>>,
    fetched: Option<Instant>,
}

/// Everything the strip learns by scanning its target pane.
pub struct Scan {
    pub found: Found,
    pub pushes: Pushes,
    pub source: String,
    pub agent_name: Option<String>,
}

/// Resolve the target agent's PRs once (used by the strip and by `scan`).
pub fn scan_once(target: &str) -> Scan {
    let info = herdr::agent_info(target);
    let agent_name = info.as_ref().and_then(|i| i.name.clone().or(i.agent.clone()));
    let mut found = Found::new();
    let mut pushes = Pushes::new();
    if let Some(path) = info.as_ref().and_then(session::transcript_path) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            detect::from_jsonl(&text, &mut found, &mut pushes);
            return Scan { found, pushes, source: format!("transcript {}", path.display()), agent_name };
        }
    }
    if let Ok(text) = herdr::read_pane(target, 3000) {
        detect::from_text(&text, &mut found, &mut pushes);
    }
    Scan { found, pushes, source: "scrollback".into(), agent_name }
}

struct App {
    cfg: Config,
    target: String,
    position: Position,
    own_pane: Option<String>,
    agent_name: Option<String>,
    source: String,
    transcript: Option<PathBuf>,
    mtime: Option<SystemTime>,
    entries: HashMap<PrRef, Entry>,
    session: Option<String>,
    agent_kind: Option<String>,
    labels: std::collections::BTreeMap<String, Label>,
    /// Show everything: merged, mentions, and PRs you marked not-mine.
    show_all: bool,
    last_counts: Option<(usize, usize, usize)>,
    opened_at: Instant,
    pushes: Pushes,
    /// Pushed branch -> (last checked, had a PR). Branches with a PR are final.
    branch_checked: HashMap<Push, (Instant, bool)>,
    table: TableState,
    hide_merged: bool,
    message: Option<String>,
    fetching: bool,
    applied_size: Option<u16>,
    tx: Sender<FetchResult>,
    rx: Receiver<FetchResult>,
    last_resolve: Option<Instant>,
    last_mtime_check: Instant,
    last_scrollback: Option<Instant>,
    last_alive: Instant,
    quit: bool,
}

impl App {
    fn new(cfg: Config, target: String, position: Position) -> Self {
        let (tx, rx) = mpsc::channel();
        let hide_merged = cfg.hide_merged;
        Self {
            cfg,
            target,
            position,
            own_pane: None,
            agent_name: None,
            source: String::new(),
            transcript: None,
            mtime: None,
            entries: HashMap::new(),
            session: None,
            agent_kind: None,
            labels: Default::default(),
            show_all: false,
            last_counts: None,
            opened_at: Instant::now(),
            pushes: Pushes::new(),
            branch_checked: HashMap::new(),
            table: TableState::default().with_selected(Some(0)),
            hide_merged,
            message: None,
            fetching: false,
            applied_size: None,
            tx,
            rx,
            last_resolve: None,
            last_mtime_check: Instant::now(),
            last_scrollback: None,
            last_alive: Instant::now(),
            quit: false,
        }
    }

    fn merge_found(&mut self, found: Found) {
        for (pr, (signal, order)) in found {
            let reason = if signal == Signal::Owned { Reason::Action } else { Reason::Chat };
            let e = self.entries.entry(pr.clone()).or_insert(Entry {
                pr,
                signal,
                reason,
                order,
                status: None,
                fetched: None,
            });
            e.signal = e.signal.max(signal);
            e.reason = e.reason.max(reason);
        }
    }

    /// Re-resolve the agent's transcript (the agent may have restarted or resumed).
    fn resolve(&mut self) {
        self.last_resolve = Some(Instant::now());
        let info = herdr::agent_info(&self.target);
        self.agent_name = info.as_ref().and_then(|i| i.name.clone().or(i.agent.clone()));
        self.agent_kind = info.as_ref().and_then(|i| i.agent.clone());
        let session = info.as_ref().and_then(|i| i.session_value.clone());
        if session != self.session {
            self.labels = session.as_deref().map(telemetry::labels_for).unwrap_or_default();
            self.session = session;
        }
        let path = info.as_ref().and_then(session::transcript_path);
        if path != self.transcript {
            self.transcript = path;
            self.mtime = None;
        }
    }

    fn rescan(&mut self, force: bool) {
        let now = Instant::now();
        if force || self.last_resolve.is_none_or(|t| now - t >= RESOLVE_EVERY) {
            self.resolve();
        }
        if let Some(path) = self.transcript.clone() {
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if force || mtime != self.mtime {
                self.mtime = mtime;
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let mut found = Found::new();
                    let mut pushes = Pushes::new();
                    detect::from_jsonl(&text, &mut found, &mut pushes);
                    self.merge_found(found);
                    self.pushes.extend(pushes);
                    self.source = "transcript".into();
                }
            }
        } else if force || self.last_scrollback.is_none_or(|t| now - t >= SCROLLBACK_EVERY) {
            self.last_scrollback = Some(now);
            if let Ok(text) = herdr::read_pane(&self.target, 3000) {
                let mut found = Found::new();
                let mut pushes = Pushes::new();
                detect::from_text(&text, &mut found, &mut pushes);
                self.merge_found(found);
                self.pushes.extend(pushes);
                self.source = "scrollback".into();
            }
        }
    }

    fn maybe_fetch(&mut self, force: bool) {
        if self.fetching {
            return;
        }
        let ttl = Duration::from_secs(self.cfg.cache_ttl_secs);
        let due: Vec<PrRef> = self
            .entries
            .values()
            .filter(|e| force || e.fetched.is_none_or(|t| t.elapsed() >= ttl))
            // Merged/closed PRs rarely change; refresh them only on demand.
            .filter(|e| {
                force
                    || e.fetched.is_none()
                    || !matches!(&e.status, Some(Ok(s)) if matches!(s.state, State::Merged | State::Closed))
            })
            .map(|e| e.pr.clone())
            .collect();
        // Pushed branches not yet checked, or checked with no PR and TTL expired
        // (the PR may be opened after the push).
        let branches: Vec<Push> = self
            .pushes
            .iter()
            .filter(|p| match self.branch_checked.get(*p) {
                None => true,
                Some((_, true)) => false,
                Some((t, false)) => force || t.elapsed() >= ttl,
            })
            .cloned()
            .collect();
        if due.is_empty() && branches.is_empty() {
            return;
        }
        self.fetching = true;
        let tel = self.cfg.telemetry.enabled;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_all(due, branches, tel).map_err(|e| e.to_string()));
        });
    }

    fn drain_fetches(&mut self) {
        while let Ok(res) = self.rx.try_recv() {
            self.fetching = false;
            match res {
                Ok(Fetched { branches, statuses }) => {
                    self.message = None;
                    let now = Instant::now();
                    for (push, prs) in branches {
                        self.branch_checked.insert(push, (now, !prs.is_empty()));
                        for pr in prs {
                            let order = self.entries.len();
                            let e = self.entries.entry(pr.clone()).or_insert(Entry {
                                pr,
                                signal: Signal::Owned,
                                reason: Reason::Push,
                                order,
                                status: None,
                                fetched: None,
                            });
                            e.signal = Signal::Owned;
                            e.reason = e.reason.max(Reason::Push);
                        }
                    }
                    for (pr, status) in statuses {
                        if let Some(e) = self.entries.get_mut(&pr) {
                            e.status = Some(status);
                            e.fetched = Some(now);
                        }
                    }
                }
                Err(msg) => self.message = Some(msg),
            }
        }
    }

    fn visible(&self) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self
            .entries
            .values()
            .filter(|e| {
                if self.show_all {
                    return true;
                }
                match self.labels.get(&e.pr.url()) {
                    Some(Label::NotMine) => return false,
                    Some(Label::Mine) => {}
                    None if !(self.cfg.show_mentioned || e.signal == Signal::Owned) => return false,
                    None => {}
                }
                !(self.hide_merged && matches!(&e.status, Some(Ok(s)) if s.state == State::Merged))
            })
            .collect();
        // Owned first, then open before closed/merged, then first-seen order.
        v.sort_by_key(|e| {
            let done = matches!(&e.status, Some(Ok(s)) if matches!(s.state, State::Merged | State::Closed));
            let mine = e.signal == Signal::Owned || self.labels.get(&e.pr.url()) == Some(&Label::Mine);
            (!mine, done, e.order)
        });
        v
    }

    fn hidden_merged(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(&e.status, Some(Ok(s)) if s.state == State::Merged))
            .count()
    }

    fn selected_url(&self) -> Option<String> {
        let v = self.visible();
        v.get(self.table.selected()?).map(|e| e.pr.url())
    }

    fn on_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.table.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.table.select_previous(),
            KeyCode::Char('m') => self.hide_merged = !self.hide_merged,
            KeyCode::Char('a') => self.show_all = !self.show_all,
            KeyCode::Char('x') => self.toggle_label(Label::NotMine),
            KeyCode::Char('p') => self.toggle_label(Label::Mine),
            KeyCode::Char('r') => {
                self.rescan(true);
                self.maybe_fetch(true);
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(url) = self.selected_url() {
                    let _ = std::process::Command::new(opener()).arg(&url).spawn();
                    self.log_pr("action", &url, serde_json::json!({ "action": "open" }));
                    self.message = Some(format!("opened {url}"));
                }
            }
            KeyCode::Char('y') => {
                if let Some(url) = self.selected_url() {
                    self.log_pr("action", &url, serde_json::json!({ "action": "copy" }));
                    self.message = Some(match copy(&url) {
                        Ok(()) => format!("copied {url}"),
                        Err(e) => format!("copy failed: {e}"),
                    });
                }
            }
            _ => {}
        }
        let n = self.visible().len();
        if let Some(i) = self.table.selected() {
            if n > 0 && i >= n {
                self.table.select(Some(n - 1));
            }
        }
    }

    fn toggle_label(&mut self, label: Label) {
        let Some(url) = self.selected_url() else { return };
        let Some(session) = self.session.clone() else {
            self.message = Some("no agent session; label not saved".into());
            return;
        };
        let next = if self.labels.get(&url) == Some(&label) { None } else { Some(label) };
        telemetry::set_label(&session, &url, next);
        match next {
            Some(l) => self.labels.insert(url.clone(), l),
            None => self.labels.remove(&url),
        };
        let name = match next {
            Some(Label::Mine) => "mine",
            Some(Label::NotMine) => "not_mine",
            None => "cleared",
        };
        self.log_pr("label", &url, serde_json::json!({ "label": name }));
        self.message = Some(match next {
            Some(Label::NotMine) => "marked not this agent's (a shows all)".into(),
            Some(Label::Mine) => "marked this agent's".into(),
            None => "label cleared".into(),
        });
    }

    fn base_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "agent": self.agent_name, "kind": self.agent_kind, "session": self.session,
        })
    }

    fn log(&self, event: &str, extra: serde_json::Value) {
        let mut f = self.base_fields();
        if let (Some(o), serde_json::Value::Object(x)) = (f.as_object_mut(), extra) {
            o.extend(x);
        }
        telemetry::log(self.cfg.telemetry.enabled, event, f);
    }

    /// Event about one PR row, with the rule that attributed it.
    fn log_pr(&self, event: &str, url: &str, extra: serde_json::Value) {
        let e = self.entries.values().find(|e| e.pr.url() == url);
        let state = e.and_then(|e| match &e.status {
            Some(Ok(s)) => Some(format!("{:?}", s.state).to_lowercase()),
            _ => None,
        });
        let mut f = serde_json::json!({
            "pr": url, "reason": e.map(|e| e.reason.as_str()), "state": state,
        });
        if let (Some(o), serde_json::Value::Object(x)) = (f.as_object_mut(), extra) {
            o.extend(x);
        }
        self.log(event, f);
    }

    /// Log a scan summary when the counts change.
    fn log_scan(&mut self) {
        let count = |r: Reason| self.entries.values().filter(|e| e.reason == r).count();
        let counts = (count(Reason::Action), count(Reason::Push), count(Reason::Chat));
        if self.last_counts == Some(counts) {
            return;
        }
        self.last_counts = Some(counts);
        self.log("scan", serde_json::json!({
            "source": self.source, "action": counts.0, "push": counts.1, "chat": counts.2,
            "pushes": self.pushes.len(),
        }));
    }

    /// Size the strip pane to its content once its pane id is known.
    fn fit(&mut self) {
        if self.own_pane.is_none() {
            self.own_pane = toggle::load_strips().get(&self.target).cloned();
        }
        let Some(own) = self.own_pane.clone() else { return };
        let want = if self.position.vertical() {
            let rows = self.visible().len().clamp(1, self.cfg.max_rows as usize) as u16;
            rows + 1 // header line
        } else {
            self.cfg.width
        };
        if self.applied_size == Some(want) {
            return;
        }
        if resize_to(&own, &self.target, self.position, want).is_ok() {
            self.applied_size = Some(want);
        }
    }
}

fn resize_to(own: &str, target: &str, pos: Position, want: u16) -> Result<()> {
    let v = herdr::run(&["pane", "layout", "--pane", own])?;
    let panes = v.pointer("/result/layout/panes").and_then(Value::as_array).context("no panes")?;
    let dim = |id: &str| -> Option<f64> {
        let p = panes.iter().find(|p| p["pane_id"] == id)?;
        let key = if pos.vertical() { "height" } else { "width" };
        p.pointer(&format!("/rect/{key}"))?.as_f64()
    };
    let (mine, theirs) = (dim(own).context("own rect")?, dim(target).context("target rect")?);
    let total = mine + theirs;
    let delta = (mine - want as f64) / total;
    if delta.abs() * total < 1.0 {
        return Ok(());
    }
    let dir = if delta > 0.0 { pos.shrink_dir() } else { pos.grow_dir() };
    herdr::run(&[
        "pane", "resize", "--pane", own, "--direction", dir, "--amount", &format!("{:.4}", delta.abs()),
    ])?;
    Ok(())
}

fn opener() -> &'static str {
    if cfg!(target_os = "macos") { "open" } else { "xdg-open" }
}

fn copy(text: &str) -> Result<()> {
    use std::io::Write;
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::piped()).spawn()?;
    child.stdin.take().context("no stdin")?.write_all(text.as_bytes())?;
    child.wait()?;
    Ok(())
}

// ---------- rendering ----------

const DIM: Style = Style::new().fg(Color::DarkGray);

fn state_span(s: &PrStatus) -> Span<'static> {
    match s.state {
        State::Open => Span::styled("open", Style::new().fg(Color::Green)),
        State::Draft => Span::styled("draft", DIM),
        State::Merged => Span::styled("merged", Style::new().fg(Color::Magenta)),
        State::Closed => Span::styled("closed", Style::new().fg(Color::Red)),
    }
}

fn ci_span(s: &PrStatus) -> Span<'static> {
    match s.ci {
        Ci::Passing => Span::styled("✓ CI", Style::new().fg(Color::Green)),
        Ci::Failing => Span::styled("✗ CI", Style::new().fg(Color::Red)),
        Ci::Pending => Span::styled("◷ CI", Style::new().fg(Color::Yellow)),
        Ci::None => Span::styled("– CI", DIM),
    }
}

fn review_span(s: &PrStatus) -> Span<'static> {
    match &s.review {
        Review::Approved => Span::styled("approved", Style::new().fg(Color::Green)),
        Review::ChangesRequested => Span::styled("changes req", Style::new().fg(Color::Red)),
        Review::Waiting(0) => Span::styled("needs review", Style::new().fg(Color::Yellow)),
        Review::Waiting(n) => Span::styled(format!("waiting {n} rev"), Style::new().fg(Color::Yellow)),
        Review::NotRequired => Span::styled("no review", DIM),
    }
}

fn merge_span(s: &PrStatus) -> Span<'static> {
    match s.merge {
        Merge::Ready => Span::styled("ready", Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Merge::Conflicts => Span::styled("conflicts", Style::new().fg(Color::Red)),
        Merge::Blocked => Span::styled("blocked", Style::new().fg(Color::Yellow)),
        Merge::Behind => Span::styled("behind", Style::new().fg(Color::Yellow)),
        Merge::Unstable => Span::styled("unstable", Style::new().fg(Color::Yellow)),
        Merge::Unknown => Span::styled("?", DIM),
        Merge::NotApplicable => Span::raw(""),
    }
}

fn render(app: &mut App, f: &mut Frame) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
    let visible = app.visible();
    let hidden = if app.hide_merged { app.hidden_merged() } else { 0 };

    let mut spans = vec![
        Span::styled(" PRs ", Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(app.agent_name.clone().unwrap_or_else(|| app.target.clone()), Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {} shown", visible.len()), DIM),
    ];
    if hidden > 0 {
        spans.push(Span::styled(format!(" · {hidden} merged hidden"), DIM));
    }
    if app.fetching {
        spans.push(Span::styled(" · refreshing…", Style::new().fg(Color::Yellow)));
    }
    if let Some(m) = &app.message {
        spans.push(Span::styled(format!(" · {m}"), Style::new().fg(Color::Yellow)));
    }
    if app.show_all {
        spans.push(Span::styled(" · showing all", Style::new().fg(Color::Yellow)));
    }
    spans.push(Span::styled("   o open  y copy  x not-mine  p mine  a all  m merged  r refresh  q close", DIM));
    f.render_widget(Paragraph::new(Line::from(spans)), head);

    if visible.is_empty() {
        let text = if app.source.is_empty() { "scanning…".to_owned() } else { format!("no PRs for this agent ({})", app.source) };
        f.render_widget(Paragraph::new(Span::styled(format!(" {text}"), DIM)), body);
        return;
    }

    let rows: Vec<TRow> = visible
        .iter()
        .map(|e| {
            let name = format!(
                "{}{}#{}",
                if e.signal == Signal::Mentioned { "~" } else { "" },
                e.pr.repo,
                e.pr.number
            );
            let name = match app.labels.get(&e.pr.url()) {
                Some(Label::NotMine) => format!("✗{name}"),
                Some(Label::Mine) => format!("✓{name}"),
                None => name,
            };
            let row = match &e.status {
                None => TRow::new(vec![Line::from(name), Line::from(Span::styled("…", DIM))]),
                Some(Err(err)) => TRow::new(vec![
                    Line::from(name),
                    Line::from(Span::styled(err.clone(), Style::new().fg(Color::Red))),
                ]),
                Some(Ok(s)) => TRow::new(vec![
                    Line::from(name),
                    Line::from(state_span(s)),
                    Line::from(ci_span(s)),
                    Line::from(if matches!(s.state, State::Merged | State::Closed) { Span::raw("") } else { review_span(s) }),
                    Line::from(merge_span(s)),
                    Line::from(vec![
                        Span::styled(format!("+{}", s.additions), Style::new().fg(Color::Green)),
                        Span::raw(" "),
                        Span::styled(format!("-{}", s.deletions), Style::new().fg(Color::Red)),
                        Span::styled(format!(" {}f", s.files), DIM),
                    ]),
                    Line::from(s.title.clone()),
                ]),
            };
            if e.signal == Signal::Mentioned { row.style(DIM) } else { row }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(24),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(13),
            Constraint::Length(9),
            Constraint::Length(14),
            Constraint::Min(10),
        ],
    )
    .column_spacing(1)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▸");
    f.render_stateful_widget(table, body, &mut app.table);
}

pub fn run() -> Result<()> {
    let target = std::env::var("AGENT_PRS_TARGET").context("AGENT_PRS_TARGET not set")?;
    let cfg = config::load();
    let position = std::env::var("AGENT_PRS_POSITION")
        .ok()
        .and_then(|p| Position::parse(&p))
        .unwrap_or(cfg.position);
    let mut app = App::new(cfg, target, position);
    let mut terminal = ratatui::init();
    let res = main_loop(&mut terminal, &mut app);
    ratatui::restore();
    Ok(res?)
}

fn main_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    app.rescan(true);
    app.log("strip_open", serde_json::json!({ "position": app.position.as_str() }));
    app.maybe_fetch(false);
    while !app.quit {
        app.drain_fetches();
        app.log_scan();
        app.fit();
        terminal.draw(|f| render(app, f))?;
        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k.code);
                }
            }
        }
        let now = Instant::now();
        if now - app.last_mtime_check >= MTIME_EVERY {
            app.last_mtime_check = now;
            app.rescan(false);
            app.maybe_fetch(false);
        }
        if now - app.last_alive >= ALIVE_EVERY {
            app.last_alive = now;
            if !herdr::pane_alive(&app.target) {
                break;
            }
        }
    }
    app.log("strip_close", serde_json::json!({ "secs": app.opened_at.elapsed().as_secs() }));
    Ok(())
}
