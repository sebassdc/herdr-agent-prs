//! Live PR state via one batched `gh api graphql` call.

use std::process::Command;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::detect::PrRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Draft,
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    Passing,
    Failing,
    Pending,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Review {
    Approved,
    ChangesRequested,
    Waiting(u64),
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    Ready,
    Conflicts,
    Blocked,
    Behind,
    Unstable,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone)]
pub struct PrStatus {
    pub title: String,
    pub state: State,
    pub ci: Ci,
    pub review: Review,
    pub merge: Merge,
    pub additions: u64,
    pub deletions: u64,
    pub files: u64,
}

const FIELDS: &str = "title state isDraft merged additions deletions changedFiles \
    reviewDecision mergeStateStatus reviewRequests { totalCount } \
    commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }";

fn gql_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

pub fn build_query(prs: &[PrRef]) -> String {
    let mut q = String::from("query {");
    for (i, pr) in prs.iter().enumerate() {
        q.push_str(&format!(
            " p{i}: repository(owner: {}, name: {}) {{ pullRequest(number: {}) {{ {FIELDS} }} }}",
            gql_str(&pr.owner),
            gql_str(&pr.repo),
            pr.number
        ));
    }
    q.push_str(" }");
    q
}

/// Fetch every PR in one request. Entries that GitHub cannot resolve (no
/// access, deleted repo) come back as `Err` with a short reason.
pub fn fetch(prs: &[PrRef]) -> Result<Vec<Result<PrStatus, String>>> {
    if prs.is_empty() {
        return Ok(Vec::new());
    }
    let out = Command::new("gh")
        .args(["api", "graphql", "-f", &format!("query={}", build_query(prs))])
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("gh CLI not found on PATH"),
        Err(e) => return Err(e.into()),
    };
    let body: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if body.get("data").is_none() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("gh api graphql failed: {}", err.lines().next().unwrap_or("no output"));
    }
    Ok((0..prs.len())
        .map(|i| {
            body.pointer(&format!("/data/p{i}/pullRequest"))
                .filter(|v| !v.is_null())
                .map(parse)
                .ok_or_else(|| "not found or no access".to_owned())
        })
        .collect())
}

fn parse(v: &Value) -> PrStatus {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("");
    let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    let state = match (s("/state"), v["isDraft"].as_bool() == Some(true)) {
        ("MERGED", _) => State::Merged,
        ("CLOSED", _) => State::Closed,
        (_, true) => State::Draft,
        _ => State::Open,
    };
    let ci = match s("/commits/nodes/0/commit/statusCheckRollup/state") {
        "SUCCESS" => Ci::Passing,
        "FAILURE" | "ERROR" => Ci::Failing,
        "PENDING" | "EXPECTED" => Ci::Pending,
        _ => Ci::None,
    };
    let review = match s("/reviewDecision") {
        "APPROVED" => Review::Approved,
        "CHANGES_REQUESTED" => Review::ChangesRequested,
        "REVIEW_REQUIRED" => Review::Waiting(n("/reviewRequests/totalCount")),
        _ if n("/reviewRequests/totalCount") > 0 => Review::Waiting(n("/reviewRequests/totalCount")),
        _ => Review::NotRequired,
    };
    let merge = if matches!(state, State::Merged | State::Closed) {
        Merge::NotApplicable
    } else {
        match s("/mergeStateStatus") {
            "CLEAN" | "HAS_HOOKS" => Merge::Ready,
            "DIRTY" => Merge::Conflicts,
            "BLOCKED" => Merge::Blocked,
            "BEHIND" => Merge::Behind,
            "UNSTABLE" => Merge::Unstable,
            "DRAFT" => Merge::NotApplicable,
            _ => Merge::Unknown,
        }
    };
    PrStatus {
        title: s("/title").to_owned(),
        state,
        ci,
        review,
        merge,
        additions: n("/additions"),
        deletions: n("/deletions"),
        files: n("/changedFiles"),
    }
}

/// PRs whose head is one of the pushed branches, one batched query.
/// Returns, per push, the PR refs found (possibly empty).
pub fn prs_for_branches(pushes: &[crate::detect::Push]) -> Result<Vec<Vec<PrRef>>> {
    if pushes.is_empty() {
        return Ok(Vec::new());
    }
    let mut q = String::from("query {");
    for (i, p) in pushes.iter().enumerate() {
        q.push_str(&format!(
            " b{i}: repository(owner: {}, name: {}) {{ pullRequests(headRefName: {}, first: 5, orderBy: {{field: CREATED_AT, direction: DESC}}) {{ nodes {{ number }} }} }}",
            gql_str(&p.owner),
            gql_str(&p.repo),
            gql_str(&p.branch)
        ));
    }
    q.push_str(" }");
    let out = Command::new("gh").args(["api", "graphql", "-f", &format!("query={q}")]).output();
    let out = match out {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("gh CLI not found on PATH"),
        Err(e) => return Err(e.into()),
    };
    let body: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if body.get("data").is_none() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("gh api graphql failed: {}", err.lines().next().unwrap_or("no output"));
    }
    Ok(pushes
        .iter()
        .enumerate()
        .map(|(i, p)| {
            body.pointer(&format!("/data/b{i}/pullRequests/nodes"))
                .and_then(Value::as_array)
                .map(|nodes| {
                    nodes
                        .iter()
                        .filter_map(|n| n["number"].as_u64())
                        .map(|number| PrRef { owner: p.owner.clone(), repo: p.repo.clone(), number })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect())
}
