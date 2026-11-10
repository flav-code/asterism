//! Who owns what. Every process and container goes to exactly one owner (a session, Docker, an app, the system),
//! and each session gets what it runs: its processes, containers, builds, and the command it is on.
use crate::docker::{COMPOSE_DIR, COMPOSE_FILES, COMPOSE_PROJECT, Container, DockerInfo, GROUP_LABELS};
use crate::host::{Host, Proc};
use crate::sessions::{Activity, SessionMeta};
use crate::text;
use rust_i18n::t;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// Processes that hold a Docker engine's virtual machine, as the OS sees them. Edit freely.
pub const VM_PROCESSES: &[&str] = &[
    "OrbStack Helper",
    "com.docker.virtualization",
    "com.docker.hyperkit",
    "com.apple.Virtualization.VirtualMachine",
    "qemu-system-aarch64",
    "qemu-system-x86_64",
    "vmmem",
    "VmmemWSL",
    "vmmemWSL",
];
/// Executables that belong to the operating system.
const SYSTEM_PATHS: &[&str] = &["/System/", "/usr/libexec/", "/usr/sbin/", "/sbin/", "/Library/Apple/", "/usr/lib/systemd/"];
const ROWS_PER_SESSION: usize = 60;
const CMD_MAX: usize = 300;

/// One sample of the machine, borrowed from the samplers for the time of an attribution.
pub struct Inputs<'sample> {
    /// Now, in ms since the epoch.
    pub at: u64,
    pub host: Host,
    pub procs: &'sample [Proc],
    pub sessions: &'sample [SessionMeta],
    /// Session id → what it is running.
    pub activities: &'sample HashMap<String, Activity>,
    pub containers: Vec<Container>,
    pub docker: Option<DockerInfo>,
    /// The home folder: shown as `~`, and never a folder that claims containers.
    pub home: &'sample str,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub at: u64,
    pub host: Host,
    pub docker: Option<DockerView>,
    /// Who uses the RAM and CPU, biggest first. A session counts everything it started, containers included.
    pub owners: Vec<Owner>,
    /// Running sessions, biggest first.
    pub sessions: Vec<SessionView>,
    /// Containers no session owns.
    pub unattached: Vec<GroupView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerKind {
    Session,
    Docker,
    App,
    System,
    /// A process outside every session and app.
    Other,
}

#[derive(Debug, Serialize)]
pub struct Owner {
    pub kind: OwnerKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Percent of one core.
    pub cpu: f32,
    pub mem: u64,
    pub procs: usize,
    /// An app's bundle (`/Applications/X.app`), for its icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DockerView {
    pub name: String,
    pub ncpu: u32,
    /// What the engine sees as its memory: the VM's size when it runs in one.
    pub mem: u64,
    pub containers: usize,
    pub containers_cpu: f32,
    pub containers_mem: u64,
    /// The VM as the host sees it, when there is one: one block the OS cannot split.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm: Option<VmView>,
}

#[derive(Debug, Serialize)]
pub struct VmView {
    pub cpu: f32,
    pub mem: u64,
    /// What the VM holds beyond its containers' own usage: builds, caches, the engine.
    pub other_mem: u64,
}

#[derive(Debug, Serialize)]
pub struct SessionView {
    pub id: String,
    pub title: String,
    pub project: String,
    pub kind: String,
    pub status: Option<String>,
    pub pid: u32,
    /// Its processes, the `claude` process and all it started.
    pub cpu: f32,
    pub mem: u64,
    /// Its containers.
    pub docker_cpu: f32,
    pub docker_mem: u64,
    pub activity: Option<ActivityView>,
    pub builds: Vec<Build>,
    pub procs: Vec<ProcRow>,
    /// Processes left out of `procs`.
    pub more: usize,
    pub groups: Vec<GroupView>,
}

#[derive(Debug, Serialize)]
pub struct ActivityView {
    pub tool: String,
    pub detail: String,
    pub since: u64,
    /// The process running it, when it could be found, with everything under it.
    pub pid: Option<u32>,
    pub cpu: Option<f32>,
    pub mem: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct Build {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub cmd: String,
    pub cpu: f32,
    pub mem: u64,
    /// Seconds.
    pub age: u64,
}

#[derive(Debug, Serialize)]
pub struct ProcRow {
    pub pid: u32,
    pub depth: usize,
    pub cpu: f32,
    pub mem: u64,
    pub age: u64,
    pub heavy: bool,
    /// Started by the session but detached from it, found by its folder.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub detached: bool,
    pub cmd: String,
}

#[derive(Debug, Serialize)]
pub struct GroupView {
    /// "Container" (in the window's language), "Compose" or "Kubernetes".
    pub kind: String,
    pub name: String,
    /// How it was attached: "client" (a session's docker command names it) or "folder" (a session's folder holds it).
    pub why: Option<&'static str>,
    pub dir: Option<String>,
    /// How many sessions tie for it, when it stays unattached because of that.
    pub shared: usize,
    pub cpu: f32,
    pub mem: u64,
    pub containers: Vec<ContainerView>,
}

#[derive(Debug, Serialize)]
pub struct ContainerView {
    pub id: String,
    pub name: String,
    pub image: String,
    pub cmd: String,
    pub cpu: f32,
    pub mem: u64,
    pub cpus: f64,
    pub mem_limit: u64,
    /// Neither CPU nor memory is limited.
    pub unlimited: bool,
    pub age: u64,
    pub heavy: bool,
}

/// Containers that share a cluster or a Compose project, or one alone.
pub struct Group {
    pub kind: String,
    pub name: String,
    pub containers: Vec<Container>,
    pub owner: Option<usize>,
    pub why: Option<&'static str>,
    pub dir: Option<String>,
    pub shared: usize,
    /// The docker command that names it, when attached by one.
    pub client: Option<String>,
}

/// What a session can claim containers by: the folders it works in, the ones it only went through,
/// and the docker commands it runs.
#[derive(Default)]
pub struct Claim {
    pub dirs: Vec<String>,
    pub visited: Vec<String>,
    pub clients: Vec<String>,
}

/// One spelling per folder: forward slashes, no trailing slash, Docker Desktop's host prefixes removed.
#[must_use]
pub fn norm_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("/host_mnt/") {
        s = format!("/{rest}"); // Docker Desktop on macOS
    }
    if let Some(rest) = s.strip_prefix("/run/desktop/mnt/host/") {
        // Docker Desktop on Windows: /run/desktop/mnt/host/c/Users/me → c:/Users/me
        // shortcut: not checked on a real Windows yet.
        let (drive, path) = rest.split_once('/').unwrap_or((rest, ""));
        s = format!("{drive}:/{path}");
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

fn inside(p: &str, dir: &str) -> bool {
    p == dir || p.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// Containers in rows: those of one Kubernetes cluster or Compose project together, the others each alone.
#[must_use]
pub fn group_containers(containers: Vec<Container>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for c in containers {
        let label = GROUP_LABELS.iter().find_map(|g| c.labels.get(g.label).map(|v| (g.kind, v.clone())));
        let key = label.as_ref().map_or_else(|| c.id.clone(), |(kind, v)| format!("{kind}:{v}"));
        let i = *index.entry(key).or_insert_with(|| {
            let (kind, name) = label.clone().map_or_else(|| (t!("core.container"), c.name.clone()), |(k, n)| (k.into(), n));
            groups.push(Group {
                kind: kind.into(),
                name,
                containers: Vec::new(),
                owner: None,
                why: None,
                dir: None,
                shared: 0,
                client: None,
            });
            groups.len() - 1
        });
        groups[i].containers.push(c);
    }
    groups
}

/// A container (or a whole cluster or Compose project) belongs to the one session whose docker command names it,
/// else to the session with the most specific folder holding one of its mounts or its Compose folder. At equal
/// length, a folder the session works in beats one its shell only went through. A tie leaves it unattached.
pub fn attach(groups: &mut [Group], claims: &[Claim]) {
    for g in groups {
        let ids: Vec<&str> = g.containers.iter().map(|c| c.id.as_str()).collect();
        let paths: Vec<String> =
            g.containers.iter().flat_map(|c| c.binds.iter().chain(c.labels.get(COMPOSE_DIR))).map(|p| norm_path(p)).collect();
        let mut names: HashSet<&str> = HashSet::new();
        for c in &g.containers {
            names.insert(&c.name);
            names.extend(c.binds.iter().map(String::as_str));
            names.extend([COMPOSE_PROJECT, COMPOSE_DIR].iter().filter_map(|k| c.labels.get(*k).map(String::as_str)));
            names.extend(c.labels.get(COMPOSE_FILES).into_iter().flat_map(|f| f.split(',')));
        }
        names.remove("");
        let hex = |t: &str| (6..=64).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let named = |cmd: &str| {
            cmd.split(|ch: char| ch.is_whitespace() || "=,:".contains(ch))
                .any(|t| !t.is_empty() && (names.contains(t) || (hex(t) && ids.iter().any(|id| id.starts_with(t)))))
        };
        let by_client: Vec<usize> =
            claims.iter().enumerate().filter(|(_, c)| c.clients.iter().any(|cmd| named(cmd))).map(|(i, _)| i).collect();
        if let [only] = by_client[..] {
            (g.owner, g.why) = (Some(only), Some("client"));
            g.client = claims[only].clients.iter().find(|cmd| named(cmd)).cloned();
            continue;
        }
        let (mut best, mut dir, mut owners) = (0, String::new(), Vec::new());
        for (i, c) in claims.iter().enumerate() {
            for (list, bonus) in [(&c.dirs, 1), (&c.visited, 0)] {
                for d in list.iter().map(|d| norm_path(d)) {
                    let score = if paths.iter().any(|p| inside(p, &d)) { d.len() * 2 + bonus } else { 0 };
                    if score > best {
                        (best, dir, owners) = (score, d, vec![i]);
                    } else if score > 0 && score == best && !owners.contains(&i) {
                        owners.push(i);
                    }
                }
            }
        }
        if let [only] = owners[..] {
            (g.owner, g.why, g.dir) = (Some(only), Some("folder"), Some(dir));
        } else if !owners.is_empty() {
            g.shared = owners.len();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Own {
    Session(usize),
    Docker,
    App(String),
    System,
    Other(String),
}

/// An app bundle (macOS): its name, and the path of its `.app` folder.
struct Bundle {
    name: String,
    path: String,
}

/// What a process and everything it started use.
#[derive(Clone, Copy, Default)]
struct Load {
    cpu: f32,
    mem: u64,
}

/// What the processes of one owner use, and how many they are.
#[derive(Default)]
struct OwnerSum {
    cpu: f32,
    mem: u64,
    procs: usize,
}

/// The app a process belongs to: the first `.app` bundle in its path (macOS).
fn bundle_of(p: &Proc) -> Option<Bundle> {
    let path = if p.exe.is_empty() { &p.cmd } else { &p.exe };
    let end = path.find(".app/")?;
    let name = path[..end].rsplit(['/', '\\']).next().filter(|n| !n.is_empty())?;
    Some(Bundle { name: name.to_string(), path: path[..end + 4].to_string() })
}

fn is_system(p: &Proc) -> bool {
    p.exe.is_empty() || SYSTEM_PATHS.iter().any(|s| p.exe.starts_with(s)) || p.exe.to_lowercase().starts_with(r"c:\windows\")
}

fn is_vm(p: &Proc) -> bool {
    let file = p.exe.rsplit(['/', '\\']).next().unwrap_or_default();
    VM_PROCESSES
        .iter()
        .any(|n| p.name.eq_ignore_ascii_case(n) || file.eq_ignore_ascii_case(n) || file.eq_ignore_ascii_case(&format!("{n}.exe")))
}

/// Says who uses the machine: each process goes to the agent session that started it, to an app, to Docker or to the
/// system, and each container to the session that runs it.
pub fn attribute(inp: Inputs) -> Report {
    let home = norm_path(inp.home);
    let now_s = inp.at / 1000;
    let by_pid: HashMap<u32, &Proc> = inp.procs.iter().map(|p| (p.pid, p)).collect();
    let mut kids: HashMap<u32, Vec<&Proc>> = HashMap::new();
    for p in inp.procs {
        if p.ppid != p.pid && by_pid.contains_key(&p.ppid) {
            kids.entry(p.ppid).or_default().push(p);
        }
    }
    for list in kids.values_mut() {
        list.sort_by_key(|p| p.pid);
    }
    let subtree = |pid: u32| {
        let (mut load, mut stack, mut seen) = (Load::default(), vec![pid], HashSet::new());
        while let Some(q) = stack.pop() {
            if !seen.insert(q) {
                continue;
            }
            if let Some(p) = by_pid.get(&q) {
                load = Load { cpu: load.cpu + p.cpu, mem: load.mem + p.mem };
            }
            stack.extend(kids.get(&q).into_iter().flatten().map(|k| k.pid));
        }
        load
    };

    // Session roots: the outermost `claude` processes (one started by another counts for the outer one).
    let registry: HashMap<u32, usize> = inp.sessions.iter().enumerate().filter_map(|(i, s)| Some((s.pid?, i))).collect();
    let is_root = |p: &Proc| (registry.contains_key(&p.pid) && p.cmd.to_lowercase().contains("claude")) || text::is_claude(&p.cmd);
    let under_root = |p: &Proc| {
        let mut q = by_pid.get(&p.ppid);
        for _ in 0..64 {
            let Some(x) = q else { return false };
            if is_root(x) {
                return true;
            }
            q = (x.ppid != x.pid).then(|| by_pid.get(&x.ppid)).flatten();
        }
        false
    };
    let mut roots: Vec<&Proc> = inp.procs.iter().filter(|p| is_root(p) && !under_root(p)).collect();
    roots.sort_by_key(|p| p.pid);

    // Each root's session: the registry's, else its --resume id, else the one that started in its folder when it did.
    let mut taken = HashSet::new();
    let mut lives: Vec<(&Proc, Option<usize>)> = Vec::new();
    let mut lost = Vec::new();
    for p in roots {
        let found =
            registry.get(&p.pid).copied().or_else(|| text::resume_of(&p.cmd).and_then(|id| inp.sessions.iter().position(|s| s.id == id)));
        match found {
            Some(i) if taken.insert(i) => lives.push((p, Some(i))),
            _ => lost.push(p),
        }
    }
    for p in lost {
        let dirs: Vec<String> = p.cwd.iter().cloned().chain(text::add_dirs(&p.cmd)).map(|d| norm_path(&d)).collect();
        let start = p.start * 1000;
        let best = inp
            .sessions
            .iter()
            .enumerate()
            .filter(|(i, s)| !taken.contains(i) && s.cwd.as_deref().is_some_and(|c| dirs.contains(&norm_path(c))))
            .filter_map(|(i, s)| Some((i, s.started_at?.abs_diff(start))))
            .filter(|(_, gap)| *gap < 300_000)
            .min_by_key(|(_, gap)| *gap)
            .map(|(i, _)| i);
        if let Some(i) = best {
            taken.insert(i);
        }
        lives.push((p, best));
    }

    let mut owner: HashMap<u32, Own> = HashMap::new();
    let claim_tree = |root: u32, own: Own, owner: &mut HashMap<u32, Own>| {
        let mut stack = vec![root];
        while let Some(q) = stack.pop() {
            if owner.contains_key(&q) {
                continue;
            }
            owner.insert(q, own.clone());
            stack.extend(kids.get(&q).into_iter().flatten().map(|k| k.pid));
        }
    };
    for (li, (root, _)) in lives.iter().enumerate() {
        claim_tree(root.pid, Own::Session(li), &mut owner);
    }

    // The folders each session works in; the home folder or above would claim everything.
    let meaningful = |d: &String| !d.is_empty() && !format!("{home}/").starts_with(&format!("{d}/"));
    let clean = |list: Vec<String>| -> Vec<String> {
        let mut out: Vec<String> = list.iter().map(|d| norm_path(d)).filter(meaningful).collect();
        out.sort();
        out.dedup();
        out
    };
    let mut claims: Vec<Claim> = lives
        .iter()
        .map(|(root, m)| {
            let m = m.map(|i| &inp.sessions[i]);
            let cwd = m.and_then(|m| m.cwd.clone()).or_else(|| root.cwd.clone());
            let dirs = cwd.into_iter().chain(text::add_dirs(&root.cmd)).chain(m.map(|m| m.dirs.clone()).unwrap_or_default()).collect();
            Claim { dirs: clean(dirs), visited: clean(m.map(|m| m.visited.clone()).unwrap_or_default()), clients: Vec::new() }
        })
        .collect();

    // Detached processes (a daemon, a `nohup` server): no parent left but launchd or init, so found by their folder.
    let mut detached: HashMap<usize, Vec<u32>> = HashMap::new();
    let orphans: Vec<&Proc> =
        inp.procs.iter().filter(|p| !owner.contains_key(&p.pid) && (p.ppid <= 1 || !by_pid.contains_key(&p.ppid)) && p.pid > 1).collect();
    for p in orphans {
        let Some(cwd) = p.cwd.as_deref().map(norm_path).filter(meaningful) else { continue };
        let (mut best, mut who) = (0, Vec::new());
        for (li, c) in claims.iter().enumerate() {
            for d in c.dirs.iter().filter(|d| inside(&cwd, d)) {
                if d.len() > best {
                    (best, who) = (d.len(), vec![li]);
                } else if d.len() == best && !who.contains(&li) {
                    who.push(li);
                }
            }
        }
        if let [li] = who[..] {
            claim_tree(p.pid, Own::Session(li), &mut owner);
            detached.entry(li).or_default().push(p.pid);
        }
    }

    // The Docker VM, then apps (a process goes to the nearest app above it), then the system, then the rest.
    for p in inp.procs.iter().filter(|p| is_vm(p)) {
        claim_tree(p.pid, Own::Docker, &mut owner);
    }
    let mut bundles: HashMap<String, String> = HashMap::new();
    for p in inp.procs {
        if owner.contains_key(&p.pid) {
            continue;
        }
        let mut app = None;
        let mut q = Some(p);
        for _ in 0..64 {
            let Some(x) = q else { break };
            if x.pid <= 1 {
                break;
            }
            if let Some(Bundle { name, path }) = bundle_of(x) {
                bundles.entry(name.clone()).or_insert(path);
                app = Some(name);
                break;
            }
            q = (x.ppid != x.pid).then(|| by_pid.get(&x.ppid).copied()).flatten();
        }
        let own = match app {
            Some(a) => Own::App(a),
            None if is_system(p) => Own::System,
            None => Own::Other(p.name.clone()),
        };
        owner.insert(p.pid, own);
    }

    // Each session's process rows (largest branches first), its builds and the docker commands it runs.
    let row = |p: &Proc, depth: usize, detached: bool, heavy: bool| ProcRow {
        pid: p.pid,
        depth,
        cpu: p.cpu,
        mem: p.mem,
        age: now_s.saturating_sub(p.start),
        heavy,
        detached,
        cmd: text::short_program(&text::redact(&text::readable(&p.cmd), inp.home, CMD_MAX)),
    };
    let mut views: Vec<SessionView> = Vec::new();
    // Per session: the docker commands under the process running its current call, whose containers it also costs.
    let mut activity_clients: Vec<Vec<String>> = Vec::new();
    for (li, (root, m)) in lives.iter().enumerate() {
        let (mut rows, mut builds, mut clients) = (Vec::new(), Vec::new(), Vec::new());
        let starts: Vec<(u32, bool)> =
            std::iter::once((root.pid, false)).chain(detached.get(&li).into_iter().flatten().map(|p| (*p, true))).collect();
        let (mut cpu, mut mem) = (0.0, 0);
        for (start, is_detached) in starts {
            let Load { cpu: c, mem: m } = subtree(start);
            (cpu, mem) = (cpu + c, mem + m);
            let mut stack = vec![(start, 0, false)];
            while let Some((pid, depth, in_build)) = stack.pop() {
                let Some(p) = by_pid.get(&pid) else { continue };
                if owner.get(&pid) != Some(&Own::Session(li)) || depth > 40 {
                    continue;
                }
                let heavy = depth > 0 && text::is_shown_build(&p.cmd);
                if heavy && !in_build {
                    let Load { cpu: c, mem: m } = subtree(pid);
                    builds.push(Build {
                        pid: Some(pid),
                        container: None,
                        cmd: text::short_program(&text::redact(&text::readable(&p.cmd), inp.home, CMD_MAX)),
                        cpu: c,
                        mem: m,
                        age: now_s.saturating_sub(p.start),
                    });
                }
                if depth > 0 && text::is_docker_cli(&p.cmd) {
                    clients.push(p.cmd.clone());
                }
                rows.push(row(p, depth, is_detached && depth == 0, heavy));
                let mut children: Vec<&Proc> = kids.get(&pid).cloned().unwrap_or_default();
                children.sort_by(|a, b| subtree(a.pid).cpu.total_cmp(&subtree(b.pid).cpu));
                stack.extend(children.into_iter().map(|k| (k.pid, depth + 1, in_build || heavy)));
            }
        }
        claims[li].clients = clients;
        let meta = m.map(|i| &inp.sessions[i]);
        let mut clients_now = Vec::new();
        let activity = meta.filter(|s| s.status.as_deref() != Some("idle")).and_then(|s| inp.activities.get(&s.id)).map(|a| {
            // The Bash call's process, as `readable` shows it: "$ <command>".
            let runner = a.command.as_deref().and_then(|cmd| {
                rows.iter().find(|r| by_pid.get(&r.pid).is_some_and(|p| text::readable(&p.cmd).strip_prefix("$ ") == Some(cmd.trim())))
            });
            let sums = runner.map(|r| subtree(r.pid));
            let mut stack: Vec<u32> = runner.map(|r| r.pid).into_iter().collect();
            while let Some(q) = stack.pop() {
                clients_now.extend(by_pid.get(&q).filter(|p| text::is_docker_cli(&p.cmd)).map(|p| p.cmd.clone()));
                stack.extend(kids.get(&q).into_iter().flatten().map(|k| k.pid));
            }
            ActivityView {
                tool: a.tool.clone(),
                detail: text::redact(&a.detail, inp.home, 200),
                since: a.since,
                pid: runner.map(|r| r.pid),
                cpu: sums.map(|s| s.cpu),
                mem: sums.map(|s| s.mem),
            }
        });
        activity_clients.push(clients_now);
        let more = rows.len().saturating_sub(ROWS_PER_SESSION);
        rows.truncate(ROWS_PER_SESSION);
        let cwd = meta.and_then(|s| s.cwd.clone()).or_else(|| root.cwd.clone()).unwrap_or_default();
        let project = meta.map(|s| s.project.clone()).filter(|p| !p.is_empty()).unwrap_or_else(|| crate::sessions::project_of(&cwd));
        views.push(SessionView {
            id: meta.map_or_else(|| format!("pid:{}", root.pid), |s| s.id.clone()),
            title: meta.map(|s| s.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| format!("claude · {project}")),
            project,
            kind: meta.map_or_else(|| "cli".into(), |s| s.kind.clone()),
            status: meta.and_then(|s| s.status.clone()),
            pid: root.pid,
            cpu,
            mem,
            docker_cpu: 0.0,
            docker_mem: 0,
            activity,
            builds,
            procs: rows,
            more,
            groups: Vec::new(),
        });
    }

    // Containers: grouped, then attached.
    let mut groups = group_containers(inp.containers);
    attach(&mut groups, &claims);
    let view = |g: &Group| GroupView {
        kind: g.kind.clone(),
        name: g.name.clone(),
        why: g.why,
        dir: g.dir.as_ref().map(|d| d.replace(&home, "~")),
        shared: g.shared,
        cpu: g.containers.iter().map(|c| c.cpu).sum(),
        mem: g.containers.iter().map(|c| c.mem).sum(),
        containers: g
            .containers
            .iter()
            .map(|c| ContainerView {
                id: c.id.chars().take(12).collect(),
                name: c.name.clone(),
                image: c.image.clone(),
                cmd: text::redact(&c.cmd, inp.home, CMD_MAX),
                cpu: c.cpu,
                mem: c.mem,
                cpus: c.cpus,
                mem_limit: c.mem_limit,
                unlimited: c.cpus == 0.0 && c.mem_limit == 0,
                age: if c.started > 0 { now_s.saturating_sub(c.started) } else { 0 },
                heavy: text::is_heavy(&c.cmd),
            })
            .collect(),
    };
    let mut unattached = Vec::new();
    for g in &groups {
        let v = view(g);
        match g.owner {
            Some(li) => {
                let s = &mut views[li];
                (s.docker_cpu, s.docker_mem) = (s.docker_cpu + v.cpu, s.docker_mem + v.mem);
                if let (Some(a), Some(client)) = (s.activity.as_mut(), &g.client)
                    && activity_clients[li].contains(client)
                {
                    (a.cpu, a.mem) = (Some(a.cpu.unwrap_or(0.0) + v.cpu), Some(a.mem.unwrap_or(0) + v.mem));
                }
                for c in v.containers.iter().filter(|c| c.heavy) {
                    s.builds.push(Build {
                        pid: None,
                        container: Some(c.name.clone()),
                        cmd: c.cmd.clone(),
                        cpu: c.cpu,
                        mem: c.mem,
                        age: c.age,
                    });
                }
                s.groups.push(v);
            }
            None => unattached.push(v),
        }
    }
    unattached.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));

    // Owners. Sessions count their containers; Docker counts its VM minus what the sessions already count.
    let mut sums: HashMap<Own, OwnerSum> = HashMap::new();
    for p in inp.procs {
        if let Some(o) = owner.get(&p.pid) {
            let sum = sums.entry(o.clone()).or_default();
            sum.cpu += p.cpu;
            sum.mem += p.mem;
            sum.procs += 1;
        }
    }
    let OwnerSum { cpu: vm_cpu, mem: vm_mem, .. } = sums.remove(&Own::Docker).unwrap_or_default();
    let containers: Vec<&Container> = groups.iter().flat_map(|g| &g.containers).collect();
    let (attached_cpu, attached_mem) = views.iter().fold((0.0, 0), |a, s| (a.0 + s.docker_cpu, a.1 + s.docker_mem));
    let mut owners: Vec<Owner> = Vec::new();
    for (o, OwnerSum { cpu, mem, procs: n }) in sums {
        let (kind, name, session) = match o {
            Own::Session(li) => {
                let s = &views[li];
                owners.push(Owner {
                    kind: OwnerKind::Session,
                    name: s.title.clone(),
                    session: Some(s.id.clone()),
                    cpu: cpu + s.docker_cpu,
                    mem: mem + s.docker_mem,
                    procs: n,
                    bundle: None,
                });
                continue;
            }
            Own::App(a) => (OwnerKind::App, a, None),
            Own::System => (OwnerKind::System, "System".to_string(), None),
            Own::Other(name) => (OwnerKind::Other, name, None),
            Own::Docker => unreachable!("removed above"),
        };
        let bundle = (kind == OwnerKind::App).then(|| bundles.get(&name).cloned()).flatten();
        owners.push(Owner { kind, name, session, cpu, mem, procs: n, bundle });
    }
    let docker_name = inp.docker.as_ref().map(|d| d.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Docker".into());
    if vm_mem > 0 || !containers.is_empty() {
        // With a VM, its host footprint holds every container; without one, containers run as host processes.
        let (cpu, mem) = if vm_mem > 0 {
            ((vm_cpu - attached_cpu).max(0.0), vm_mem.saturating_sub(attached_mem))
        } else {
            (containers.iter().map(|c| c.cpu).sum::<f32>() - attached_cpu, containers.iter().map(|c| c.mem).sum::<u64>() - attached_mem)
        };
        let name = if vm_mem > 0 { format!("{docker_name} VM") } else { docker_name.clone() };
        owners.push(Owner { kind: OwnerKind::Docker, name, session: None, cpu, mem, procs: 0, bundle: None });
    }
    owners.retain(|o| o.mem > 0 || o.cpu > 0.0);
    owners.sort_by(|a, b| b.mem.cmp(&a.mem).then(b.cpu.total_cmp(&a.cpu)));
    views.sort_by_key(|s| Reverse(s.mem + s.docker_mem));

    let containers_mem: u64 = containers.iter().map(|c| c.mem).sum();
    let docker = (inp.docker.is_some() || !containers.is_empty()).then(|| DockerView {
        name: docker_name,
        ncpu: inp.docker.as_ref().map_or(0, |d| d.ncpu),
        mem: inp.docker.as_ref().map_or(0, |d| d.mem),
        containers: containers.len(),
        containers_cpu: containers.iter().map(|c| c.cpu).sum(),
        containers_mem,
        vm: (vm_mem > 0).then(|| VmView { cpu: vm_cpu, mem: vm_mem, other_mem: vm_mem.saturating_sub(containers_mem) }),
    });
    Report { at: inp.at, host: inp.host, docker, owners, sessions: views, unattached }
}
