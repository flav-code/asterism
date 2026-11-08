//! The session map: every session (desktop, terminal, subagent, or only named by a message), who started or forked
//! whom, and every message they sent each other. Plus token usage over every transcript, read in the background.
use crate::desktop::DesktopSession;
use crate::sessions::project_of;
use crate::transcripts::{Move, Transcripts};
use crate::usage::{Biggest, PRICES, Totals, Usage, family, model_id};
use rust_i18n::t;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Before the full scan is done, transcripts of sessions idle for longer than this are not read.
const SCAN_DAYS: u64 = 14;
/// Transcripts read per call while the full scan runs, newest first.
/// Bytes of transcripts read per build until all were read once: little for the first build, so the map shows
/// within a fraction of a second, then more each build.
const FIRST_READ: u64 = 8 << 20;
const READ_BUDGET: u64 = 96 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Desktop,
    Cli,
    Agent,
    /// Named by a message, nowhere else.
    Ghost,
    /// On another machine.
    Remote,
}

#[derive(Clone, Debug, Serialize)]
pub struct PrView {
    pub n: u64,
    pub url: String,
    pub state: String,
    pub repo: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Tok {
    pub o: u64,
    pub t: u64,
    pub usd: f64,
}

impl Tok {
    fn add(self, t: &Totals) -> Tok {
        Tok { o: self.o + t.o, t: self.t + t.all(), usd: self.usd + t.usd }
    }
    fn plus(self, t: Tok) -> Tok {
        Tok { o: self.o + t.o, t: self.t + t.t, usd: self.usd + t.usd }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub title: String,
    pub project: String,
    pub cwd: Option<String>,
    pub created_at: u64,
    pub last_at: u64,
    pub archived: bool,
    pub live: bool,
    /// busy, idle, offline (closed), done (a finished subagent), remote, gone.
    pub status: String,
    /// When a live session's status last changed, in ms (0: unknown).
    pub status_at: u64,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub branch: Option<String>,
    pub turns: u64,
    pub scheduled: bool,
    pub prs: Vec<PrView>,
    /// A subagent's session.
    pub parent: Option<String>,
    pub agent_type: Option<String>,
    pub now: Option<Move>,
    pub tok: Option<Tok>,
    pub pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    Spawn,
    Fork,
    Agent,
}

#[derive(Clone, Debug, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MsgKind {
    Message,
    /// A session reporting back to the one that started it.
    Report,
}

#[derive(Clone, Debug, Serialize)]
pub struct Msg {
    pub id: String,
    pub from: String,
    pub to: String,
    pub ts: u64,
    pub summary: String,
    pub text: String,
    pub ok: Option<bool>,
    pub note: String,
    pub kind: MsgKind,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Graph {
    pub at: u64,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub messages: Vec<Msg>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DayRow {
    pub d: String,
    pub model: String,
    pub fam: &'static str,
    pub agent: bool,
    pub t: Totals,
    /// What the cache reads would have cost as plain input.
    pub saved: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct HourRow {
    pub h: String,
    pub fam: &'static str,
    pub t: Totals,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionUsage {
    pub id: String,
    pub title: String,
    pub project: String,
    pub kind: NodeKind,
    /// Day → [output tokens, $, all tokens, $ spent by its subagents].
    pub days: BTreeMap<String, [f64; 4]>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UsageView {
    /// Every transcript has been read once.
    pub ready: bool,
    pub scanned: usize,
    pub total: usize,
    /// Bytes of transcripts read so far, of all there are: how far the first reading is.
    pub read_bytes: u64,
    pub total_bytes: u64,
    pub first: u64,
    pub biggest: Option<Biggest>,
    pub days: Vec<DayRow>,
    pub hours: Vec<HourRow>,
    /// (minute start in ms, totals), the last two hours.
    pub minutes: Vec<(u64, Totals)>,
    pub sessions: Vec<SessionUsage>,
    pub tools: Vec<(String, u64)>,
    pub unpriced: Vec<(String, u64)>,
}

/// `to` shapes: `local_<uuid>`, `uds:<socket>`, `bridge:<id>`, `a<16 hex>` (a subagent), `<title>` or `<title> [hex6]`.
pub fn resolve_target(to: &str, by_socket: &HashMap<String, String>, by_title: &HashMap<String, String>) -> Option<String> {
    let hex = |s: &str| s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if let Some(rest) = to.strip_prefix("local_")
        && rest.len() == 36
        && rest.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
    {
        return Some(to.to_string());
    }
    if let Some(socket) = to.strip_prefix("uds:") {
        return by_socket.get(socket).cloned();
    }
    if to.starts_with("bridge:") {
        return by_socket.get(to).cloned();
    }
    if to.len() == 17 && to.starts_with('a') && hex(&to[1..]) {
        return Some(format!("agent:{to}"));
    }
    by_title.get(&title_key(to)).cloned()
}

/// A title as messages name it: without a trailing " [hex6]", trimmed, lowercase.
fn title_key(t: &str) -> String {
    let t = t.trim();
    let t = match t.rsplit_once(" [") {
        Some((head, tail)) if tail.len() == 7 && tail.ends_with(']') && tail[..6].bytes().all(|b| b.is_ascii_hexdigit()) => head,
        _ => t,
    };
    t.trim().to_lowercase()
}

fn modified_ms(p: &Path) -> u64 {
    fs::metadata(p).ok().and_then(|m| m.modified().ok()).and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64)
}

fn created_ms(p: &Path) -> u64 {
    fs::metadata(p)
        .ok()
        .and_then(|m| m.created().or_else(|_| m.modified()).ok())
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

fn entries(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path())
}

/// A live-session registry entry (`~/.claude/sessions/<pid>.json`); the file may outlive its process.
struct Registered {
    pid: u32,
    session: String,
    cwd: Option<String>,
    started: u64,
    updated: u64,
    status: Option<String>,
    status_at: u64,
    host: Option<String>,
    names: Vec<String>,
    socket: Option<String>,
    bridge: Option<String>,
}

fn registry(home: &Path) -> Vec<Registered> {
    entries(&home.join(".claude/sessions"))
        .filter_map(|f| serde_json::from_slice::<Value>(&fs::read(f).ok()?).ok())
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
            let u = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
            let former = r.get("formerNames").and_then(Value::as_array).into_iter().flatten();
            let names =
                s("name").into_iter().chain(former.filter_map(|f| f.get("name").unwrap_or(f).as_str().map(str::to_string))).collect();
            Some(Registered {
                pid: u32::try_from(r.get("pid")?.as_u64()?).ok()?,
                session: s("sessionId")?,
                cwd: s("cwd"),
                started: u("startedAt"),
                updated: u("updatedAt"),
                status: s("status"),
                status_at: u("statusUpdatedAt"),
                host: s("hostSessionId"),
                names,
                socket: s("messagingSocketPath"),
                bridge: s("bridgeSessionId"),
            })
        })
        .collect()
}

/// A subagent of a transcript: `<transcript>/subagents/agent-<id>.meta.json` and `.jsonl`.
struct Subagent {
    id: String,
    title: Option<String>,
    agent_type: Option<String>,
    created: u64,
    last: u64,
    transcript: PathBuf,
}

fn subagents(transcript: &Path) -> Vec<Subagent> {
    let dir = transcript.with_extension("").join("subagents");
    entries(&dir)
        .filter_map(|meta| {
            let name = meta.file_name()?.to_str()?;
            let id = name.strip_prefix("agent-")?.strip_suffix(".meta.json")?.to_string();
            let m: Value = serde_json::from_slice(&fs::read(&meta).ok()?).unwrap_or_default();
            let jsonl = dir.join(format!("agent-{id}.jsonl"));
            let last = modified_ms(&jsonl).max(modified_ms(&meta));
            Some(Subagent {
                title: m.get("description").and_then(Value::as_str).map(str::to_string),
                agent_type: m.get("agentType").and_then(Value::as_str).map(str::to_string),
                created: created_ms(&meta),
                last,
                transcript: jsonl,
                id,
            })
        })
        .collect()
}

fn node(id: &str, kind: NodeKind, title: &str, project: &str) -> Node {
    Node {
        id: id.into(),
        kind,
        title: title.into(),
        project: project.into(),
        cwd: None,
        created_at: 0,
        last_at: 0,
        archived: false,
        live: false,
        status: "offline".into(),
        status_at: 0,
        model: None,
        profile: None,
        branch: None,
        turns: 0,
        scheduled: false,
        prs: Vec::new(),
        parent: None,
        agent_type: None,
        now: None,
        tok: None,
        pid: None,
    }
}

/// Builds the map and keeps token usage up to date. Keep one and call `build` at a steady pace: transcripts are read
/// from where they stopped, and the full history is read in steps between calls.
#[derive(Default)]
pub struct Visualizer {
    pub transcripts: Transcripts,
    pub usage: Usage,
    /// Every transcript (main and subagent), newest first, for the full scan.
    all: Vec<PathBuf>,
    scanned: usize,
    ready: bool,
    /// Transcript id → path, for the main transcripts.
    index: HashMap<String, PathBuf>,
    /// The last map: who each transcript belongs to, for usage per session.
    cli_to_node: HashMap<String, String>,
    builds: u64,
    /// The size of every transcript, measured until all were read once.
    total_bytes: u64,
    titles: HashMap<String, (String, String, NodeKind)>,
}

impl Visualizer {
    fn list(&mut self, home: &Path) {
        let projects = home.join(".claude/projects");
        let mut all = Vec::new();
        self.index.clear();
        for dir in entries(&projects) {
            for f in entries(&dir) {
                if f.extension().is_some_and(|e| e == "jsonl") {
                    if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
                        self.index.insert(stem.to_string(), f.clone());
                    }
                    all.push(f);
                } else if f.is_dir() {
                    all.extend(entries(&f.join("subagents")).filter(|a| a.extension().is_some_and(|e| e == "jsonl")));
                }
            }
        }
        if !self.ready {
            all.sort_by_cached_key(|f| std::cmp::Reverse(modified_ms(f)));
            self.total_bytes = all.iter().map(|f| fs::metadata(f).map_or(0, |m| m.len())).sum();
        }
        self.all = all;
    }

    /// One step of reading every transcript: the first pass within `budget`, newest files first, then every file again
    /// for what it gained.
    fn full_scan_step(&mut self, now_ms: u64, budget: &mut u64) {
        if self.ready {
            for f in &self.all {
                self.transcripts.scan(f, &mut self.usage, now_ms);
            }
        } else {
            while self.scanned < self.all.len() && *budget > 0 {
                let f = self.all[self.scanned].clone();
                if !self.transcripts.scan_within(&f, &mut self.usage, now_ms, budget).is_none_or(|t| t.complete()) {
                    break;
                }
                self.scanned += 1;
            }
        }
        self.ready = self.scanned >= self.all.len();
        self.usage.trim_minutes(now_ms);
    }

    pub fn build(&mut self, home: &Path, desktop: &[DesktopSession], live: &HashSet<u32>, now_ms: u64) -> Graph {
        self.list(home);
        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut reports: Vec<Msg> = Vec::new();
        let mut cli: HashMap<String, Vec<String>> = HashMap::new(); // node → its transcript ids
        let mut titles: HashMap<String, Vec<String>> = HashMap::new();
        let mut cli_to_node: HashMap<String, String> = HashMap::new();
        let mut by_socket: HashMap<String, String> = HashMap::new();
        // Read once per build: the check below compares with the same text the node was given.
        let (untitled, cli_title) = (t!("core.untitled_session"), t!("core.cli_session"));

        for d in desktop {
            let mut n = node(&d.id, NodeKind::Desktop, if d.title.is_empty() { &untitled } else { &d.title }, "");
            n.project = project_of(d.origin_cwd.as_deref().or(d.cwd.as_deref()).unwrap_or("?"));
            (n.cwd, n.created_at, n.last_at, n.archived) = (d.cwd.clone(), d.created_at, d.last_activity_at, d.archived);
            (n.model, n.profile, n.branch, n.turns, n.scheduled) =
                (d.model.clone(), Some(d.profile.clone()), d.branch.clone(), d.turns, d.scheduled);
            n.prs = d.prs.iter().map(|p| PrView { n: p.n, url: p.url.clone(), state: p.state.clone(), repo: p.repo.clone() }).collect();
            let ids: Vec<String> = d.cli.iter().chain(&d.prior_cli).cloned().collect();
            for c in &ids {
                cli_to_node.insert(c.clone(), d.id.clone());
            }
            cli.insert(d.id.clone(), ids);
            titles.insert(d.id.clone(), std::iter::once(d.title.clone()).chain(d.previous_titles.iter().cloned()).collect());
            for b in &d.bridges {
                by_socket.insert(format!("bridge:{b}"), d.id.clone());
            }
            if let Some(from) = &d.spawned_from {
                edges.push(Edge { from: from.clone(), to: d.id.clone(), kind: EdgeKind::Spawn, at: d.created_at });
            }
            if let Some(from) = &d.forked_from {
                edges.push(Edge { from: from.clone(), to: d.id.clone(), kind: EdgeKind::Fork, at: d.created_at });
            }
            for note in &d.notes {
                reports.push(Msg {
                    // The note's key is shared by notes from several sessions (a batch): the sender and time make it unique.
                    id: format!("r:{}:{}:{}:{}", d.id, note.from, note.key, note.at),
                    from: note.from.clone(),
                    to: d.id.clone(),
                    ts: note.at,
                    summary: note.title.clone(),
                    text: note.text.clone(),
                    ok: None,
                    note: String::new(),
                    kind: MsgKind::Report,
                });
            }
            order.push(d.id.clone());
            nodes.insert(d.id.clone(), n);
        }

        for r in registry(home) {
            let id = r.host.clone().filter(|h| nodes.contains_key(h)).unwrap_or_else(|| format!("cli:{}", r.session));
            let n = nodes.entry(id.clone()).or_insert_with(|| {
                order.push(id.clone());
                let mut n = node(
                    &id,
                    NodeKind::Cli,
                    r.names.first().map_or(&*cli_title, String::as_str),
                    &project_of(r.cwd.as_deref().unwrap_or("?")),
                );
                (n.cwd, n.created_at, n.last_at) = (r.cwd.clone(), r.started, r.updated);
                n
            });
            titles.entry(id.clone()).or_default().extend(r.names.iter().cloned());
            cli.entry(id.clone()).or_default().push(r.session.clone());
            cli_to_node.insert(r.session.clone(), id.clone());
            if let Some(b) = &r.bridge {
                by_socket.insert(format!("bridge:{b}"), id.clone());
            }
            if let Some(s) = &r.socket {
                by_socket.insert(s.clone(), id.clone());
            }
            if live.contains(&r.pid) {
                n.live = true;
                n.status = r.status.clone().unwrap_or_else(|| "idle".into());
                n.status_at = r.status_at;
                n.pid = Some(r.pid);
                n.last_at = n.last_at.max(r.updated);
            }
        }

        // Titles name sessions in messages; on a clash the live, most recent one wins (it is set last).
        let mut by_title: HashMap<String, String> = HashMap::new();
        let mut ranked: Vec<&Node> = nodes.values().collect();
        ranked.sort_by_key(|n| (n.live, n.last_at));
        for n in ranked {
            for t in titles.get(&n.id).into_iter().flatten().filter(|t| !t.is_empty()) {
                by_title.insert(title_key(t), n.id.clone());
            }
        }

        // Transcripts: messages, latest move, tokens; subagents. Before the full scan, recent sessions only.
        let cutoff = if self.ready { 0 } else { now_ms.saturating_sub(SCAN_DAYS * 86_400_000) };
        let mut budget = match (self.ready, self.builds) {
            (true, _) => u64::MAX,
            (false, 0) => FIRST_READ,
            _ => READ_BUDGET,
        };
        self.builds += 1;
        let mut messages: Vec<Msg> = Vec::new();
        // Open and recent sessions first, so what the map shows first fills in first.
        let mut first: Vec<String> = order.clone();
        first.sort_by_key(|id| std::cmp::Reverse((nodes[id].live, nodes[id].last_at)));
        for id in first {
            let (is_live, last_at) = (nodes[&id].live, nodes[&id].last_at);
            if !is_live && last_at < cutoff {
                continue;
            }
            for c in cli.get(&id).cloned().unwrap_or_default() {
                let Some(path) = self.index.get(&c).cloned() else { continue };
                let Some(st) = self.transcripts.scan_within(&path, &mut self.usage, now_ms, &mut budget) else { continue };
                let (tokens, last, title) = (st.tokens.total, st.last.clone(), st.title.clone());
                messages.extend(st.sent.iter().map(|s| Msg {
                    id: s.id.clone(),
                    from: id.clone(),
                    to: s.to.clone(),
                    ts: s.ts,
                    summary: s.summary.clone(),
                    text: s.text.clone(),
                    ok: s.ok,
                    note: s.note.clone(),
                    kind: MsgKind::Message,
                }));
                let mut agents = Vec::new();
                for a in subagents(&path) {
                    let atok = self
                        .transcripts
                        .scan_within(&a.transcript, &mut self.usage, now_ms, &mut budget)
                        .map(|t| Tok::default().add(&t.tokens.total));
                    agents.push((a, atok));
                }
                let n = nodes.get_mut(&id).expect("listed above");
                if tokens.n > 0 {
                    n.tok = Some(n.tok.unwrap_or_default().add(&tokens));
                }
                if let Some(m) = last {
                    n.last_at = n.last_at.max(m.ts);
                    if n.now.as_ref().is_none_or(|old| m.ts > old.ts) {
                        n.now = Some(m);
                    }
                }
                if n.kind == NodeKind::Cli
                    && n.title == cli_title
                    && let Some(t) = title
                {
                    n.title = t;
                }
                let (project, archived) = (n.project.clone(), n.archived);
                for (a, atok) in agents {
                    if let Some(t) = atok {
                        let n = nodes.get_mut(&id).expect("listed above");
                        n.tok = Some(n.tok.unwrap_or_default().plus(t));
                    }
                    let aid = format!("agent:{}", a.id);
                    let running = now_ms.saturating_sub(a.last) < 90_000;
                    let mut an = node(&aid, NodeKind::Agent, &a.title.unwrap_or_else(|| a.id.chars().take(7).collect()), &project);
                    (an.parent, an.agent_type, an.tok) = (Some(id.clone()), Some(a.agent_type.unwrap_or_else(|| "agent".into())), atok);
                    (an.created_at, an.last_at, an.live, an.archived) = (a.created, a.last, running, archived);
                    an.status = if running { "busy" } else { "done" }.into();
                    edges.push(Edge { from: id.clone(), to: aid.clone(), kind: EdgeKind::Agent, at: a.created });
                    if !nodes.contains_key(&aid) {
                        order.push(aid.clone());
                    }
                    nodes.insert(aid, an);
                }
            }
        }

        // Resumed and forked sessions copy their history: keep one copy of each message, owned by the oldest session
        // (the real sender), preferring a copy that has its result.
        let mut by_id: HashMap<String, Msg> = HashMap::new();
        for m in messages {
            let created = |id: &str| nodes.get(id).map_or(0, |n| n.created_at);
            let better = match by_id.get(&m.id) {
                None => true,
                Some(o) => (o.ok.is_none() && m.ok.is_some()) || (o.ok.is_none() == m.ok.is_none() && created(&m.from) < created(&o.from)),
            };
            if better {
                by_id.insert(m.id.clone(), m);
            }
        }
        let mut resolved = Vec::new();
        let mut kept: Vec<Msg> = by_id.into_values().collect();
        kept.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
        for mut m in kept {
            let to = match resolve_target(&m.to, &by_socket, &by_title) {
                Some(t) => t,
                None if m.ok == Some(false) => continue, // failed sends to unknown names would only add junk nodes
                None => {
                    // The id keeps the English name, so it stays the same when the language changes.
                    let (key, name) = if let Some(sock) = m.to.strip_prefix("uds:") {
                        let pid = sock.rsplit('/').next().and_then(|f| f.strip_suffix(".sock")).unwrap_or("?");
                        (format!("CLI session · pid {pid}"), t!("core.cli_session_pid", pid = pid).into_owned())
                    } else if m.to.starts_with("bridge:") {
                        ("Remote Control session".into(), t!("core.remote_control_session").into_owned())
                    } else {
                        let name = m.to.rsplit_once(" [").filter(|(_, t)| t.len() == 7).map_or(m.to.clone(), |(h, _)| h.to_string());
                        (name.clone(), name)
                    };
                    let id = format!("ghost:{key}");
                    if !nodes.contains_key(&id) {
                        let remote = m.note.contains("another machine") || m.note.contains("Remote Control");
                        let mut g = node(&id, if remote { NodeKind::Remote } else { NodeKind::Ghost }, &name, "remote");
                        (g.created_at, g.last_at, g.status) = (m.ts, m.ts, "remote".into());
                        order.push(id.clone());
                        nodes.insert(id.clone(), g);
                    }
                    id
                }
            };
            if !nodes.contains_key(&to) {
                let project = nodes.get(&m.from).map_or_else(|| "?".to_string(), |n| n.project.clone());
                let n = if let Some(agent) = to.strip_prefix("agent:") {
                    let mut a = node(&to, NodeKind::Agent, &agent.chars().take(7).collect::<String>(), &project);
                    (a.parent, a.agent_type, a.status) = (Some(m.from.clone()), Some("agent".into()), "done".into());
                    a
                } else {
                    // The title the result quoted, as in `… ("Some title")`.
                    let quoted = m.note.split_once("(\"").and_then(|(_, r)| r.split_once("\")")).map(|(t, _)| t.to_string());
                    let mut g = node(&to, NodeKind::Ghost, &quoted.unwrap_or_else(|| to.chars().take(14).collect()), &project);
                    g.status = "gone".into();
                    g
                };
                order.push(to.clone());
                nodes.insert(to.clone(), n);
            }
            let t = nodes.get_mut(&to).expect("inserted above");
            t.last_at = t.last_at.max(m.ts);
            if t.created_at == 0 || t.created_at > m.ts {
                t.created_at = m.ts;
            }
            m.to = to;
            resolved.push(m);
        }
        resolved.extend(reports.into_iter().filter(|r| nodes.contains_key(&r.from)));
        resolved.sort_by_key(|m| m.ts);
        edges.retain(|e| nodes.contains_key(&e.from) && nodes.contains_key(&e.to));

        self.full_scan_step(now_ms, &mut budget);
        self.cli_to_node = cli_to_node;
        self.titles = nodes.values().map(|n| (n.id.clone(), (n.title.clone(), n.project.clone(), n.kind))).collect();
        let nodes = order.into_iter().filter_map(|id| nodes.remove(&id)).collect();
        Graph { at: now_ms, nodes, edges, messages: resolved }
    }

    /// Token usage as the dashboard shows it.
    pub fn usage_view(&self) -> UsageView {
        let u = &self.usage;
        let days = u
            .day
            .iter()
            .map(|((d, model, agent), t)| {
                let saved = PRICES.iter().find(|p| p.0 == model_id(model)).map_or(0.0, |p| t.cr as f64 * (p.1 - p.3) / 1e6);
                DayRow { d: d.clone(), model: model.clone(), fam: family(model), agent: *agent, t: *t, saved }
            })
            .collect();
        let hours = u.hour.iter().map(|((h, fam), t)| HourRow { h: h.clone(), fam, t: *t }).collect();
        let minutes = u.minute.iter().map(|(m, t)| (m * 60_000, *t)).collect();
        let mut sessions: HashMap<String, SessionUsage> = HashMap::new();
        for (path, st) in &self.transcripts.files {
            if st.tokens.total.n == 0 {
                continue;
            }
            // A subagent's tokens go to its session: <id>/subagents/agent-x.jsonl → <id>.jsonl.
            let own = if st.agent { path.parent().and_then(Path::parent).map(|p| p.with_extension("jsonl")) } else { Some(path.clone()) };
            let Some(own) = own else { continue };
            let uuid = own.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
            let id = self.cli_to_node.get(&uuid).cloned().unwrap_or_else(|| format!("cli:{uuid}"));
            let entry = sessions.entry(id.clone()).or_insert_with(|| {
                let ost = self.transcripts.files.get(&own);
                let (title, project, kind) = self.titles.get(&id).cloned().unwrap_or_else(|| {
                    let title = ost
                        .and_then(|o| o.title.clone().or_else(|| o.tokens.slug.clone()))
                        .unwrap_or_else(|| uuid.chars().take(8).collect());
                    // The session's own folder, not a subagent's (which may work elsewhere).
                    let cwd = ost.and_then(|o| o.tokens.cwd.as_deref()).or(st.tokens.cwd.as_deref());
                    let project = project_of(cwd.unwrap_or("?"));
                    (title, project, NodeKind::Cli)
                });
                SessionUsage { id, title, project, kind, days: BTreeMap::new() }
            });
            for (d, t) in &st.tokens.days {
                let x = entry.days.entry(d.clone()).or_default();
                x[0] += t.o as f64;
                x[1] += t.usd;
                x[2] += t.all() as f64;
                if st.agent {
                    x[3] += t.usd;
                }
            }
        }
        let mut tools: Vec<(String, u64)> = u.tools.iter().map(|(k, v)| (k.clone(), *v)).collect();
        tools.sort_by_key(|t| std::cmp::Reverse(t.1));
        tools.truncate(14);
        UsageView {
            ready: self.ready,
            scanned: self.scanned,
            total: self.all.len(),
            read_bytes: if self.ready { self.total_bytes } else { self.transcripts.read_bytes().min(self.total_bytes) },
            total_bytes: self.total_bytes,
            first: u.first,
            biggest: u.biggest.clone(),
            days,
            hours,
            minutes,
            sessions: sessions.into_values().collect(),
            tools,
            unpriced: u.unpriced.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_targets() {
        let by_socket = HashMap::from([("/tmp/cc-socks/1.sock".to_string(), "cli:x".to_string())]);
        let by_title = HashMap::from([("refonte page".to_string(), "local_b".to_string())]);
        let r = |to: &str| resolve_target(to, &by_socket, &by_title);
        assert_eq!(r("local_0404acf9-4d7f-436d-99f9-17995b3e8077").as_deref(), Some("local_0404acf9-4d7f-436d-99f9-17995b3e8077"));
        assert_eq!(r("uds:/tmp/cc-socks/1.sock").as_deref(), Some("cli:x"));
        assert_eq!(r("uds:/tmp/cc-socks/2.sock"), None);
        assert_eq!(r("a7ee53c2f1788ffba").as_deref(), Some("agent:a7ee53c2f1788ffba"));
        assert_eq!(r("Refonte page [81b0b4]").as_deref(), Some("local_b"));
        assert_eq!(r("Refonte Page").as_deref(), Some("local_b"));
        assert_eq!(r("nobody"), None);
    }
}
