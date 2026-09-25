mod config;
mod detect;
mod github;
mod herdr;
mod session;
mod strip;
mod telemetry;
mod toggle;

use anyhow::{Result, bail};

const USAGE: &str = "usage: herdr-agent-prs <toggle [--pane ID] [--position top|bottom|left|right] | strip | scan <pane> | stats>";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("toggle") => {
            let mut pane = None;
            let mut pos = None;
            let mut it = args.iter().skip(1);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--pane" => pane = it.next().cloned(),
                    "--position" => pos = it.next().and_then(|p| config::Position::parse(p)),
                    other => bail!("unknown arg {other}\n{USAGE}"),
                }
            }
            toggle::run(pane, pos)
        }
        Some("strip") => strip::run(),
        Some("scan") => scan(args.get(1).map(String::as_str)),
        Some("stats") => telemetry::stats(),
        // Exit 2 with usage: scripts/build.sh uses this as a "binary runs" probe.
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

/// Debug/verification: print what the strip would show for a pane.
fn scan(pane: Option<&str>) -> Result<()> {
    let Some(pane) = pane else { bail!(USAGE) };
    let s = strip::scan_once(pane);
    println!("agent: {}", s.agent_name.as_deref().unwrap_or("-"));
    println!("source: {}", s.source);
    let mut prs: Vec<_> = s.found.iter().map(|(k, v)| (k.clone(), v.0)).collect();
    let pushes: Vec<_> = s.pushes.iter().cloned().collect();
    for (push, found) in pushes.iter().zip(github::prs_for_branches(&pushes)?) {
        println!("pushed: {}/{}:{} -> {:?}", push.owner, push.repo, push.branch, found.iter().map(|p| p.number).collect::<Vec<_>>());
        for pr in found {
            match prs.iter_mut().find(|(p, _)| *p == pr) {
                Some(e) => e.1 = detect::Signal::Owned,
                None => prs.push((pr, detect::Signal::Owned)),
            }
        }
    }
    let refs: Vec<_> = prs.iter().map(|(p, _)| p.clone()).collect();
    let statuses = github::fetch(&refs)?;
    for ((pr, sig), st) in prs.iter().zip(statuses) {
        match st {
            Ok(s) => println!(
                "{:?}\t{}\t{:?}\tci={:?}\treview={:?}\tmerge={:?}\t+{} -{} {}f\t{}",
                sig, pr.url(), s.state, s.ci, s.review, s.merge, s.additions, s.deletions, s.files, s.title
            ),
            Err(e) => println!("{sig:?}\t{}\tERR {e}", pr.url()),
        }
    }
    Ok(())
}
