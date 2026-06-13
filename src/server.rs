//! CapyFun automation server.
//!
//! Two event sources feed one trigger model derived from a monorepo's IR:
//!
//! - **GH Archive** ([gharchive.org](https://www.gharchive.org/)) — polled on a
//!   schedule; the primary firehose for public upstreams.
//! - **GitHub App webhooks** — a `/webhook` endpoint for low-latency events from
//!   repos that install the app (the future path; HMAC verification is a TODO).
//!
//! Events are *hints*: a matched event yields a [`Trigger`] naming the affected
//! target(s). Acting on a trigger (the reconcile) is the engine's job and is
//! idempotent, so this server can be lossy and at-least-once. See
//! `docs/design/automation.md`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use git2::Repository;
use serde::Serialize;

use crate::ir::Ir;
use crate::reconcile;

/// A GitHub activity event, normalized across GH Archive and webhook shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// `PushEvent`, `CreateEvent`, `ReleaseEvent`, …
    pub kind: String,
    /// `owner/name`.
    pub repo: String,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
}

/// What kind of CapyFun rule a subscription belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubKind {
    Import,
    Vendor,
}

/// A target subscribed to a repo's activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub label: String,
    pub kind: SubKind,
    /// Tracked ref for history-importing targets (used to match `PushEvent`).
    pub git_ref: Option<String>,
}

/// `owner/name` → subscriptions, built from a monorepo's IR. This is the
/// per-monorepo slice of the global reverse index described in the design doc.
#[derive(Debug, Default)]
pub struct Index(pub BTreeMap<String, Vec<Subscription>>);

impl Index {
    pub fn from_ir(ir: &Ir) -> Index {
        let mut map: BTreeMap<String, Vec<Subscription>> = BTreeMap::new();
        for i in &ir.imports {
            map.entry(i.repo.clone()).or_default().push(Subscription {
                label: i.label.clone(),
                kind: SubKind::Import,
                git_ref: Some(i.git_ref.clone()),
            });
        }
        for v in &ir.vendors {
            map.entry(v.repo.clone()).or_default().push(Subscription {
                label: v.label.clone(),
                kind: SubKind::Vendor,
                git_ref: None,
            });
        }
        Index(map)
    }

    /// Number of distinct upstream repos subscribed to.
    pub fn repos(&self) -> usize {
        self.0.len()
    }
}

/// A reconcile trigger: an [`Event`] matched a [`Subscription`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Trigger {
    pub label: String,
    pub repo: String,
    pub event: String,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
}

/// Match an event against the index, producing a trigger per relevant target.
///
/// History-importing targets react to a `PushEvent` on their tracked ref;
/// pinned vendors react to a new tag/release (a pin-bump candidate).
pub fn match_event(index: &Index, ev: &Event) -> Vec<Trigger> {
    let Some(subs) = index.0.get(&ev.repo) else {
        return Vec::new();
    };
    subs.iter()
        .filter(|s| match s.kind {
            SubKind::Import => {
                ev.kind == "PushEvent" && ev.git_ref.as_deref() == s.git_ref.as_deref()
            }
            SubKind::Vendor => ev.kind == "CreateEvent" || ev.kind == "ReleaseEvent",
        })
        .map(|s| Trigger {
            label: s.label.clone(),
            repo: ev.repo.clone(),
            event: ev.kind.clone(),
            git_ref: ev.git_ref.clone(),
            sha: ev.sha.clone(),
        })
        .collect()
}

/// Decides what happens when events match subscriptions.
///
/// Events are *hints*: the server matches them to [`Trigger`]s and hands them to
/// an `Actor`. Splitting acting behind a trait keeps the HTTP/poll plumbing
/// testable without a monorepo and lets a deployment choose report-only vs. an
/// actual reconcile.
pub trait Actor: Send + Sync {
    /// React to the triggers produced by one event/batch. Returns a short
    /// human-readable status, also used as the webhook response body.
    fn act(&self, triggers: &[Trigger]) -> String;
}

/// An actor that only *reports* matches and never touches the monorepo. Used by
/// the HTTP-layer test and any read-only deployment.
pub struct ReportOnly;

impl Actor for ReportOnly {
    fn act(&self, triggers: &[Trigger]) -> String {
        format!("{} trigger(s)", triggers.len())
    }
}

/// An actor that *reconciles* each triggered target through the shared idempotent
/// [`crate::reconcile`] path — the level-triggered loop: a hint wakes a reconcile
/// that converges the target against the commit map, so missed/duplicate events
/// are harmless. Git writes are serialized by `lock` so the poll loop and the
/// webhook handler never race on the monorepo's refs.
pub struct ReconcileActor {
    ir: Ir,
    root: PathBuf,
    lock: Mutex<()>,
}

impl ReconcileActor {
    pub fn new(ir: Ir, root: PathBuf) -> Self {
        Self {
            ir,
            root,
            lock: Mutex::new(()),
        }
    }
}

impl Actor for ReconcileActor {
    fn act(&self, triggers: &[Trigger]) -> String {
        // Distinct target labels: one event can match several targets, and a
        // batch can name the same target many times — reconcile each once.
        let mut labels: Vec<&str> = triggers.iter().map(|t| t.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        if labels.is_empty() {
            return "0 target(s) reconciled".to_owned();
        }

        // Serialize so the poll loop and webhook handler don't both write refs.
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let repo = match Repository::open(&self.root) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("reconcile: opening monorepo {}: {e}", self.root.display());
                return format!("error opening monorepo: {e}");
            }
        };

        let mut ok = 0usize;
        for label in &labels {
            match reconcile::reconcile_label(
                &repo,
                &self.ir,
                &self.root,
                label,
                reconcile::Options::default(),
            ) {
                Ok(summary) => {
                    ok += 1;
                    println!("reconcile {label}: {summary}");
                }
                Err(e) => eprintln!("reconcile {label}: error: {e:#}"),
            }
        }
        format!("{ok}/{} target(s) reconciled", labels.len())
    }
}

/// Parse one GH Archive event object into an [`Event`].
pub fn parse_archive_event(v: &serde_json::Value) -> Option<Event> {
    let kind = v.get("type")?.as_str()?.to_owned();
    let repo = v.get("repo")?.get("name")?.as_str()?.to_owned();
    let payload = v.get("payload");
    let git_ref = payload
        .and_then(|p| p.get("ref"))
        .and_then(|r| r.as_str())
        .map(str::to_owned);
    let sha = payload
        .and_then(|p| p.get("head"))
        .and_then(|h| h.as_str())
        .map(str::to_owned);
    Some(Event {
        kind,
        repo,
        git_ref,
        sha,
    })
}

/// Parse a GitHub *webhook* push payload into an [`Event`] (the future App path).
pub fn parse_webhook_push(v: &serde_json::Value) -> Option<Event> {
    let repo = v.get("repository")?.get("full_name")?.as_str()?.to_owned();
    Some(Event {
        kind: "PushEvent".to_owned(),
        repo,
        git_ref: v.get("ref").and_then(|r| r.as_str()).map(str::to_owned),
        sha: v.get("after").and_then(|h| h.as_str()).map(str::to_owned),
    })
}

/// Scan GH Archive JSON-lines, returning `(events scanned, triggers)`.
pub fn scan_archive<R: BufRead>(reader: R, index: &Index) -> Result<(usize, Vec<Trigger>)> {
    let mut scanned = 0usize;
    let mut triggers = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        scanned += 1;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(ev) = parse_archive_event(&v) {
            triggers.extend(match_event(index, &ev));
        }
    }
    Ok((scanned, triggers))
}

/// The GH Archive hourly URL for a UTC `(year, month, day, hour)`. The hour is
/// not zero-padded, matching GH Archive's filenames (e.g. `…-2024-01-01-7.json.gz`).
pub fn archive_url(year: i32, month: u32, day: u32, hour: u32) -> String {
    format!("https://data.gharchive.org/{year:04}-{month:02}-{day:02}-{hour}.json.gz")
}

/// Civil UTC `(year, month, day, hour)` from a Unix timestamp (Howard Hinnant's
/// `civil_from_days`).
pub fn utc_parts(unix_secs: i64) -> (i32, u32, u32, u32) {
    let days = unix_secs.div_euclid(86_400);
    let hour = (unix_secs.rem_euclid(86_400) / 3600) as u32;
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day, hour)
}

/// Fetch and gunzip a GH Archive hour, returning a line reader over its events.
fn fetch_archive(url: &str) -> Result<impl BufRead> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("fetching {url}"))?;
    let reader = resp.into_body().into_reader();
    Ok(BufReader::new(flate2::read::GzDecoder::new(reader)))
}

/// Run one poll cycle: fetch the most recent settled GH Archive hour, match it
/// against `index`, and hand the triggers to `actor`.
fn poll_once(index: &Index, actor: &dyn Actor) -> Result<()> {
    // GH Archive publishes hourly with a lag; look ~2 hours back to be safe.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let (y, m, d, h) = utc_parts(now - 2 * 3600);
    let url = archive_url(y, m, d, h);
    eprintln!("poll: scanning {url}");
    let (scanned, triggers) = scan_archive(fetch_archive(&url)?, index)?;
    let status = actor.act(&triggers);
    println!(
        "{url}: scanned {scanned} events, {} trigger(s); {status}",
        triggers.len()
    );
    Ok(())
}

/// Handle one HTTP request: `/healthz`, or `POST /webhook` (the future App path).
fn handle_request(mut req: tiny_http::Request, index: &Index, actor: &dyn Actor) {
    use tiny_http::Method;
    let (code, body) = match (req.method(), req.url()) {
        (Method::Get, "/healthz") => (200, "ok\n".to_owned()),
        (Method::Post, "/webhook") => {
            // TODO: verify the X-Hub-Signature-256 HMAC before trusting payloads.
            let mut buf = String::new();
            let _ = req.as_reader().read_to_string(&mut buf);
            match serde_json::from_str::<serde_json::Value>(&buf)
                .ok()
                .and_then(|v| parse_webhook_push(&v))
            {
                Some(ev) => {
                    let triggers = match_event(index, &ev);
                    eprintln!(
                        "webhook: {} {} -> {} trigger(s)",
                        ev.repo,
                        ev.git_ref.as_deref().unwrap_or(""),
                        triggers.len()
                    );
                    let status = actor.act(&triggers);
                    (202, format!("{status}\n"))
                }
                None => (400, "could not parse webhook payload\n".to_owned()),
            }
        }
        _ => (404, "not found\n".to_owned()),
    };
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code));
}

/// Serve the HTTP endpoint loop (blocking) over `server`, acting via `actor`.
pub fn run_http(server: tiny_http::Server, index: Arc<Index>, actor: Arc<dyn Actor>) {
    for req in server.incoming_requests() {
        handle_request(req, &index, actor.as_ref());
    }
}

/// Run the automation server: an HTTP endpoint plus a GH Archive poll loop.
/// Matched events drive an idempotent reconcile of the affected target(s).
/// With `once`, run a single poll cycle and return (no HTTP server).
pub fn serve(ir: &Ir, root: &Path, addr: &str, interval: Duration, once: bool) -> Result<()> {
    let index = Arc::new(Index::from_ir(ir));
    let actor: Arc<dyn Actor> = Arc::new(ReconcileActor::new(ir.clone(), root.to_path_buf()));
    eprintln!(
        "capyfun serve: {} subscribed repo(s), {} import(s), {} vendor(s)",
        index.repos(),
        ir.imports.len(),
        ir.vendors.len()
    );

    if once {
        return poll_once(&index, actor.as_ref());
    }

    let server =
        tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    eprintln!("capyfun serve: listening on http://{addr} (POST /webhook, GET /healthz)");
    let http_index = Arc::clone(&index);
    let http_actor = Arc::clone(&actor);
    std::thread::spawn(move || run_http(server, http_index, http_actor));

    loop {
        if let Err(e) = poll_once(&index, actor.as_ref()) {
            eprintln!("poll error: {e:#}");
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests;
