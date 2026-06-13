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

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use git2::Repository;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::engine::{AgentRunner, LiveRunner};
use crate::forge::{Forge, GitHubAppForge, LocalForge};
use crate::ir::Ir;
use crate::react::{self, IssueEvent, ReactionIndex};
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

/// Runs matched issue reactions for the webhook handler: resolves the affected
/// [`Reaction`](crate::ir::Reaction)s and runs each through [`react::run_reaction`]
/// against a [`Forge`] + [`AgentRunner`]. Behind its own type so the HTTP layer
/// stays testable and so a deployment can omit reactions entirely (no App
/// configured). Git/agent work is serialized by `lock` — reactions clone+push
/// external repos and run a coding agent, so one at a time keeps it cheap and
/// avoids interleaving heavy runs.
pub struct ReactionService {
    ir: Ir,
    root: PathBuf,
    forge: Box<dyn Forge>,
    runner: Box<dyn AgentRunner + Send + Sync>,
    lock: Mutex<()>,
}

impl ReactionService {
    pub fn new(
        ir: Ir,
        root: PathBuf,
        forge: Box<dyn Forge>,
        runner: Box<dyn AgentRunner + Send + Sync>,
    ) -> Self {
        Self {
            ir,
            root,
            forge,
            runner,
            lock: Mutex::new(()),
        }
    }

    /// Run every reaction matching `ev`, returning a one-line status.
    fn handle(&self, ev: &IssueEvent) -> String {
        let index = ReactionIndex::from_ir(&self.ir);
        let matched = react::match_issue(&index, ev);
        if matched.is_empty() {
            return format!("no reaction for {} issue #{}", ev.repo, ev.number);
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut summaries = Vec::new();
        for r in matched {
            match react::run_reaction(
                self.forge.as_ref(),
                self.runner.as_ref(),
                &self.ir,
                r,
                ev,
                &self.root,
            ) {
                Ok(outcome) => {
                    println!("{}", outcome.summary);
                    summaries.push(outcome.summary);
                }
                Err(e) => {
                    eprintln!("reaction {}: error: {e:#}", r.label);
                    summaries.push(format!("reaction {} error: {e}", r.label));
                }
            }
        }
        summaries.join("; ")
    }
}

/// A bounded FIFO set of recently-seen delivery keys, so a redelivered webhook
/// does not re-run the (idempotent but expensive) reconcile/reaction pipeline.
struct Dedup {
    seen: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl Dedup {
    fn new(cap: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// Record `key`; returns `true` if it was newly inserted (not a duplicate).
    fn insert_new(&mut self, key: &str) -> bool {
        if self.seen.contains(key) {
            return false;
        }
        if self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        self.order.push_back(key.to_owned());
        self.seen.insert(key.to_owned());
        true
    }
}

/// Everything the HTTP webhook/health endpoint needs: the push reverse index +
/// reconcile [`Actor`], the optional issue [`ReactionService`], the webhook HMAC
/// secret, and the delivery-dedup cache.
pub struct HttpCtx {
    index: Index,
    actor: Arc<dyn Actor>,
    reactions: Option<ReactionService>,
    /// The webhook HMAC secret. `None` makes the endpoint **fail closed** — it
    /// rejects every webhook POST — so an unsigned endpoint is never live.
    secret: Option<String>,
    seen: Mutex<Dedup>,
}

impl HttpCtx {
    /// A context with no reactions and no secret (fail-closed webhook). Tests
    /// layer a secret/reactions on with the `with_*` setters.
    pub fn new(index: Index, actor: Arc<dyn Actor>) -> Self {
        Self {
            index,
            actor,
            reactions: None,
            secret: None,
            seen: Mutex::new(Dedup::new(1024)),
        }
    }

    pub fn with_secret(mut self, secret: Option<String>) -> Self {
        self.secret = secret;
        self
    }

    pub fn with_reactions(mut self, reactions: Option<ReactionService>) -> Self {
        self.reactions = reactions;
        self
    }
}

/// Compute the `sha256=<hex>` GitHub webhook signature for `body` under `secret`.
/// Exposed so a sender/relay (and the tests) can produce the header the endpoint
/// verifies.
pub fn webhook_signature(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    format!("sha256={}", hex_encode(&mac.finalize().into_bytes()))
}

/// Verify a GitHub `X-Hub-Signature-256` header against `body` under `secret`,
/// using a constant-time compare. Rejects a missing or non-`sha256=` signature.
fn verify_signature(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    let Some(hex) = header.and_then(|h| h.strip_prefix("sha256=")) else {
        return false;
    };
    let Some(expected) = hex_decode(hex) else {
        return false;
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Read a request header by (case-insensitive) name.
fn header_value(req: &tiny_http::Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_owned())
}

/// Handle one HTTP request: `GET /healthz`, or `POST /webhook` (push reconciles
/// and issue reactions). Webhook POSTs are HMAC-verified and deduped first.
fn handle_request(mut req: tiny_http::Request, ctx: &HttpCtx) {
    use tiny_http::Method;

    // Only `/webhook` needs the body/headers; read them up front for that route.
    let (code, body) = match (req.method(), req.url()) {
        (Method::Get, "/healthz") => (200, "ok\n".to_owned()),
        (Method::Post, "/webhook") => {
            let mut raw = Vec::new();
            let _ = req.as_reader().read_to_end(&mut raw);
            let sig = header_value(&req, "X-Hub-Signature-256");
            let event = header_value(&req, "X-GitHub-Event").unwrap_or_default();
            let delivery = header_value(&req, "X-GitHub-Delivery");
            handle_webhook(ctx, &raw, sig.as_deref(), &event, delivery.as_deref())
        }
        _ => (404, "not found\n".to_owned()),
    };
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code));
}

/// The `POST /webhook` logic, factored out for unit testing without a socket:
/// HMAC-verify → dedupe → route by `X-GitHub-Event`. Returns `(status, body)`.
fn handle_webhook(
    ctx: &HttpCtx,
    raw: &[u8],
    signature: Option<&str>,
    event: &str,
    delivery: Option<&str>,
) -> (u16, String) {
    // Authenticate first: fail closed when no secret is configured.
    let Some(secret) = ctx.secret.as_deref() else {
        return (401, "webhook secret not configured; endpoint disabled\n".to_owned());
    };
    if !verify_signature(secret, raw, signature) {
        return (401, "invalid or missing signature\n".to_owned());
    }

    // Dedupe exact redeliveries by GitHub's delivery id.
    if let Some(id) = delivery {
        let mut seen = ctx.seen.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.insert_new(id) {
            return (200, "duplicate delivery; skipped\n".to_owned());
        }
    }

    let Ok(v) = serde_json::from_slice::<serde_json::Value>(raw) else {
        return (400, "could not parse webhook payload\n".to_owned());
    };

    match event {
        "ping" => (200, "pong\n".to_owned()),
        "issues" => match react::parse_webhook_issue(&v) {
            Some(ev) => match &ctx.reactions {
                Some(service) => {
                    eprintln!(
                        "webhook: issues {} #{} {} -> reacting",
                        ev.repo, ev.number, ev.action
                    );
                    (202, format!("{}\n", service.handle(&ev)))
                }
                None => (202, "reactions not configured; ignored\n".to_owned()),
            },
            None => (400, "could not parse issues payload\n".to_owned()),
        },
        // `push` (and any other event) routes to the reconcile path.
        _ => match parse_webhook_push(&v) {
            Some(ev) => {
                let triggers = match_event(&ctx.index, &ev);
                eprintln!(
                    "webhook: {} {} -> {} trigger(s)",
                    ev.repo,
                    ev.git_ref.as_deref().unwrap_or(""),
                    triggers.len()
                );
                let status = ctx.actor.act(&triggers);
                (202, format!("{status}\n"))
            }
            None => (400, "could not parse webhook payload\n".to_owned()),
        },
    }
}

/// Serve the HTTP endpoint loop (blocking) over `server`, using `ctx`.
pub fn run_http(server: tiny_http::Server, ctx: Arc<HttpCtx>) {
    for req in server.incoming_requests() {
        handle_request(req, &ctx);
    }
}

/// Build the issue [`ReactionService`] from the environment, or `None` when there
/// are no reactions or no way to act. Live deployments authenticate as a GitHub
/// App (`CAPYFUN_GITHUB_APP_ID`/`_KEY`); hermetic demos use local bare repos
/// (`CAPYFUN_GITHUB_BASE`).
fn build_reaction_service(ir: &Ir, root: &Path) -> Option<ReactionService> {
    if ir.reactions.is_empty() {
        return None;
    }
    let forge: Box<dyn Forge> = if std::env::var("CAPYFUN_GITHUB_APP_ID").is_ok() {
        match GitHubAppForge::from_env() {
            Ok(f) => Box::new(f),
            Err(e) => {
                eprintln!("capyfun serve: reactions disabled: {e:#}");
                return None;
            }
        }
    } else if std::env::var("CAPYFUN_GITHUB_BASE").is_ok() {
        Box::new(LocalForge::new())
    } else {
        eprintln!(
            "capyfun serve: {} reaction(s) declared but no GitHub App configured \
             (set CAPYFUN_GITHUB_APP_ID + CAPYFUN_GITHUB_APP_KEY); reactions disabled",
            ir.reactions.len()
        );
        return None;
    };
    Some(ReactionService::new(
        ir.clone(),
        root.to_path_buf(),
        forge,
        Box::new(LiveRunner),
    ))
}

/// Run the automation server: an HTTP endpoint plus a GH Archive poll loop.
/// Matched push events drive an idempotent reconcile; matched issue events drive
/// a reaction. With `once`, run a single poll cycle and return (no HTTP server).
pub fn serve(ir: &Ir, root: &Path, addr: &str, interval: Duration, once: bool) -> Result<()> {
    let index = Index::from_ir(ir);
    let actor: Arc<dyn Actor> = Arc::new(ReconcileActor::new(ir.clone(), root.to_path_buf()));
    eprintln!(
        "capyfun serve: {} subscribed repo(s), {} import(s), {} vendor(s), {} reaction(s)",
        index.repos(),
        ir.imports.len(),
        ir.vendors.len(),
        ir.reactions.len(),
    );

    if once {
        return poll_once(&index, actor.as_ref());
    }

    let secret = std::env::var("CAPYFUN_WEBHOOK_SECRET").ok();
    if secret.is_none() {
        eprintln!(
            "capyfun serve: CAPYFUN_WEBHOOK_SECRET is not set; the webhook endpoint will \
             reject all POSTs (fail closed). /healthz and polling are unaffected."
        );
    }
    let reactions = build_reaction_service(ir, root);

    let server =
        tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    eprintln!("capyfun serve: listening on http://{addr} (POST /webhook, GET /healthz)");

    // The poll loop reuses the same reconcile actor (one lock) as the webhook
    // path, so the two never race on the monorepo's refs.
    let poll_index = Index::from_ir(ir);
    let poll_actor = Arc::clone(&actor);

    let ctx = Arc::new(
        HttpCtx::new(index, actor)
            .with_secret(secret)
            .with_reactions(reactions),
    );
    std::thread::spawn(move || run_http(server, ctx));

    loop {
        if let Err(e) = poll_once(&poll_index, poll_actor.as_ref()) {
            eprintln!("poll error: {e:#}");
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests;
