//! Other machines (VMs, servers) whose sessions show beside this computer's. Each is reached with the system's `ssh`,
//! the way its user reaches it from a terminal (keys, ~/.ssh/config, the SSH agent), and never with a password. A
//! small agent runs there (`asterism-cli serve`), reads its ~/.claude and sends the map and usage back as patches
//! (`asterism_core::remote`). It is copied there over the same connection the first time and for each new version,
//! into ~/.cache/asterism; nothing else on the machine changes.
use crate::settings::Machine;
use asterism_core::graph::{self, Graph, UsageView};
use asterism_core::remote::{Frame, Mirror};
use rust_i18n::t;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    /// Copying the agent there.
    Installing,
    Connected,
    /// What went wrong, as ssh or the machine said it.
    Failed(String),
}

#[derive(Clone)]
struct Remote {
    status: Status,
    graph: Option<Arc<Graph>>,
    usage: Option<Arc<UsageView>>,
}

/// Each machine's latest state, by name.
static REMOTES: LazyLock<Mutex<HashMap<String, Remote>>> = LazyLock::new(Default::default);

/// The connection of each machine, by name: what it was started with, and how to stop it.
struct Worker {
    machine: Machine,
    lang: String,
    stop: Arc<AtomicBool>,
    ssh: Arc<Mutex<Option<Child>>>,
}

static WORKERS: LazyLock<Mutex<HashMap<String, Worker>>> = LazyLock::new(Default::default);

/// Locks one of this module's mutexes.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("poisoned only if a thread of this module panicked while holding it, which is a bug to fix there")
}

/// Why a machine can't be added as given (a text for the person adding it), against the names already taken.
pub fn check(name: &str, target: &str, taken: &[Machine]) -> Result<(), String> {
    let (name, target) = (name.trim(), target.trim());
    if name.is_empty() {
        return Err(t!("machines.check.name").into_owned());
    }
    if taken.iter().any(|m| m.name == name) {
        return Err(t!("machines.check.taken").into_owned());
    }
    if target.is_empty() {
        return Err(t!("machines.check.target").into_owned());
    }
    // ssh would read "-oProxyCommand=…" as an option: a target is a host, maybe with a user and a port.
    if target.starts_with('-') || target.chars().any(|c| c.is_whitespace() || c.is_control() || "'\"`$;&|<>\\".contains(c)) {
        return Err(t!("machines.check.not_target").into_owned());
    }
    Ok(())
}

/// Starts a connection for each machine of the list and stops those no longer in it (or changed).
pub fn sync(machines: &[Machine], lang: &str) {
    let mut workers = locked(&WORKERS);
    workers.retain(|name, w| {
        let keep = w.lang == lang && machines.iter().any(|m| m == &w.machine);
        if !keep {
            w.stop.store(true, Ordering::Relaxed);
            if let Some(child) = locked(&w.ssh).as_mut() {
                child.kill().ok();
            }
            locked(&REMOTES).remove(name);
        }
        keep
    });
    let new: Vec<Machine> = machines.iter().filter(|m| !workers.contains_key(&m.name)).cloned().collect();
    for m in &new {
        let w = Worker { machine: m.clone(), lang: lang.to_string(), stop: Arc::default(), ssh: Arc::default() };
        let (machine, lang, stop, ssh) = (w.machine.clone(), w.lang.clone(), w.stop.clone(), w.ssh.clone());
        locked(&REMOTES).insert(m.name.clone(), Remote { status: Status::Connecting, graph: None, usage: None });
        std::thread::Builder::new()
            .name(format!("machine {}", m.name))
            .spawn(move || follow(&machine, &lang, &stop, &ssh))
            .expect("a thread per machine");
        workers.insert(m.name.clone(), w);
    }
}

/// A machine's state, and how many sessions it has on the map.
pub struct MachineState {
    pub status: Status,
    pub sessions: usize,
}

/// A machine's state now; none for a machine that was never added.
pub fn status(name: &str) -> Option<MachineState> {
    let remotes = locked(&REMOTES);
    let r = remotes.get(name)?;
    let sessions = r.graph.as_ref().map_or(0, |g| g.nodes.iter().filter(|n| crate::state::is_session(n)).count());
    Some(MachineState { status: r.status.clone(), sessions })
}

fn set(name: &str, f: impl FnOnce(&mut Remote)) {
    if let Some(r) = locked(&REMOTES).get_mut(name) {
        f(r);
    }
}

/// This computer's map and usage with the other machines'. The usage stays the same `Arc` while nothing in it
/// changed, so the usage tab computes it again only when it must.
pub fn merged(g: Arc<Graph>, u: Arc<UsageView>) -> (Arc<Graph>, Arc<UsageView>) {
    /// The last merged usage, and the usages it was made of (their addresses).
    #[derive(Default)]
    struct Merged {
        made_of: Vec<usize>,
        usage: Option<Arc<UsageView>>,
    }
    static USAGE: LazyLock<Mutex<Merged>> = LazyLock::new(Default::default);
    let remotes = locked(&REMOTES);
    let mut names: Vec<&String> = remotes.keys().collect();
    names.sort();
    let graphs: Vec<(&str, &Graph)> = names.iter().filter_map(|n| Some((n.as_str(), &**remotes[*n].graph.as_ref()?))).collect();
    let usages: Vec<&Arc<UsageView>> = names.iter().filter_map(|n| remotes[*n].usage.as_ref()).collect();
    let g = if graphs.is_empty() { g } else { Arc::new(graph::merge_graphs(&g, &graphs)) };
    if usages.is_empty() {
        return (g, u);
    }
    let key: Vec<usize> = std::iter::once(Arc::as_ptr(&u) as usize).chain(usages.iter().map(|u| Arc::as_ptr(u) as usize)).collect();
    let mut cache = locked(&USAGE);
    if cache.made_of != key || cache.usage.is_none() {
        let others: Vec<&UsageView> = usages.iter().map(|u| &***u).collect();
        *cache = Merged { made_of: key, usage: Some(Arc::new(graph::merge_usage(&u, &others))) };
    }
    (g, cache.usage.clone().expect("set above"))
}

/// Keeps one machine connected until it is removed: connects, installs the agent when it is missing, and after a
/// failure tries again a little later each time (up to a minute).
fn follow(m: &Machine, lang: &str, stop: &AtomicBool, ssh: &Mutex<Option<Child>>) {
    let mut wait = Duration::from_secs(2);
    while !stop.load(Ordering::Relaxed) {
        set(&m.name, |r| r.status = Status::Connecting);
        let started = Instant::now();
        let ended = session(m, lang, stop, ssh);
        // Its sessions leave the map until it is back: nobody knows what they do now. What they cost stays.
        set(&m.name, |r| r.graph = None);
        let failed = match ended {
            Ok(None) => None,
            Ok(Some(Platform { system, processor })) => {
                set(&m.name, |r| r.status = Status::Installing);
                match install(&m.target, &system, &processor) {
                    Ok(()) => continue,
                    Err(e) => Some(e),
                }
            }
            Err(e) => Some(e),
        };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if started.elapsed() > Duration::from_secs(60) {
            wait = Duration::from_secs(2); // it worked for a while: a new problem, not the same one again
        }
        let e = failed.unwrap_or_else(|| t!("machines.error.closed").into_owned());
        set(&m.name, |r| r.status = Status::Failed(e));
        let until = Instant::now() + wait;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
        }
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

/// `ssh` to `target`, never asking anything: a password or an unknown host key fails with what ssh says.
fn ssh(target: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args(["-T", "-C", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]);
    c.args(["--", target]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up for each connection
    }
    c
}

/// The command run there, through `sh` whatever the login shell: the agent of this version if it is there, else the
/// system and processor it would need.
fn remote_command(lang: &str) -> String {
    // This computer's time zone, so the machine's days and hours are the same as here.
    let tz = iana_time_zone::get_timezone()
        .ok()
        .filter(|z| !z.is_empty() && z.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c)))
        .map_or(String::new(), |z| format!("TZ={z} "));
    let lang: String = lang.chars().filter(char::is_ascii_alphabetic).collect();
    format!(
        "sh -c 'a=\"$HOME/.cache/asterism/agent-{VERSION}\"; if [ -x \"$a\" ]; then {tz}exec \"$a\" serve --lang {lang}; fi; \
         echo asterism-install $(uname -s) $(uname -m)'"
    )
}

/// What a machine runs, as its `uname` says it.
struct Platform {
    system: String,
    processor: String,
}

/// One connection, until it ends. Ok(Some(platform)) when the agent must be installed first.
fn session(m: &Machine, lang: &str, stop: &AtomicBool, slot: &Mutex<Option<Child>>) -> Result<Option<Platform>, String> {
    let mut child = ssh(&m.target)
        .arg(remote_command(lang))
        .spawn()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { t!("machines.error.no_ssh").into_owned() } else { e.to_string() })?;
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    *locked(slot) = Some(child);
    let said = Arc::new(Mutex::new(String::new()));
    let said_too = said.clone();
    let errors = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                *locked(&said_too) = line.trim().to_string();
            }
        }
    });
    let (mut mirror, mut install) = (Mirror::default(), None);
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(rest) = line.strip_prefix("asterism-install ") {
            let mut parts = rest.split_whitespace().map(str::to_string);
            install = Some(Platform { system: parts.next().unwrap_or_default(), processor: parts.next().unwrap_or_default() });
            break;
        }
        // Whatever the login scripts print before the agent starts is not ours.
        let Ok(frame) = serde_json::from_str::<Frame>(&line) else { continue };
        let (map, usage) = (matches!(frame, Frame::Map { .. }), matches!(frame, Frame::Usage { .. }));
        mirror.apply(frame);
        let (g, u) = (map.then(|| Arc::new(mirror.graph.clone())), usage.then(|| Arc::new(mirror.usage.clone())));
        set(&m.name, |r| {
            r.status = Status::Connected;
            r.graph = g.or(r.graph.take());
            r.usage = u.or(r.usage.take());
        });
    }
    if let Some(mut c) = locked(slot).take() {
        c.kill().ok();
        c.wait().ok();
    }
    errors.join().ok();
    let said = locked(&said).clone();
    if install.is_some() || stop.load(Ordering::Relaxed) || said.is_empty() { Ok(install) } else { Err(explain(&said, &m.target)) }
}

/// What ssh said, with what to do about it for the usual cases.
fn explain(said: &str, target: &str) -> String {
    let hint = if said.contains("Host key verification failed") {
        t!("machines.hint.host_key", target = target)
    } else if said.contains("Permission denied") {
        t!("machines.hint.permission", target = target)
    } else {
        return said.to_string();
    };
    format!("{said} — {hint}")
}

/// Copies the agent for this system and processor to the machine, over SSH, into ~/.cache/asterism (where earlier
/// versions of it go).
fn install(target: &str, os: &str, arch: &str) -> Result<(), String> {
    if os != "Linux" {
        return Err(t!("machines.error.os", os = os).into_owned());
    }
    let arch = match arch {
        "x86_64" | "amd64" => "x64",
        "aarch64" | "arm64" => "arm64",
        other => return Err(t!("machines.error.arch", arch = other).into_owned()),
    };
    let agent = agent_binary(arch)?;
    let script = format!(
        "sh -c 'mkdir -p \"$HOME/.cache/asterism\" && cd \"$HOME/.cache/asterism\" && cat > agent.part && chmod 755 agent.part \
         && mv -f agent.part agent-{VERSION} && for f in agent-*; do [ \"$f\" = agent-{VERSION} ] || rm -f \"$f\"; done'"
    );
    let mut child = ssh(target).arg(script).spawn().map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().expect("piped");
    let wrote = stdin.write_all(&agent);
    drop(stdin);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if wrote.is_err() || !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).lines().rfind(|l| !l.trim().is_empty()).unwrap_or_default().trim().to_string();
        return Err(t!("machines.error.install", why = explain(&said, target)).into_owned());
    }
    Ok(())
}

/// The agent for Linux on `arch`: from ASTERISM_AGENT_DIR when set (builds being tried out), else this version's
/// release on GitHub, checked against the SHA-256 GitHub publishes and kept for the next machine.
fn agent_binary(arch: &str) -> Result<Vec<u8>, String> {
    let name = format!("asterism-agent-{VERSION}-linux-{arch}");
    if let Ok(dir) = std::env::var("ASTERISM_AGENT_DIR") {
        return std::fs::read(Path::new(&dir).join(&name)).map_err(|e| format!("{name}: {e}"));
    }
    let kept = crate::settings::data_dir().join("agents").join(&name);
    if let Ok(bytes) = std::fs::read(&kept) {
        return Ok(bytes);
    }
    let fail = |e: String| t!("machines.error.download", why = e).into_owned();
    let ua = format!("Asterism/{VERSION}");
    let api = format!("https://api.github.com/repos/{}/releases/tags/v{VERSION}", crate::update::REPO);
    let release: serde_json::Value = crate::update::agent()
        .get(&api)
        .header("User-Agent", &ua)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| fail(e.to_string()))?
        .body_mut()
        .read_json()
        .map_err(|e| fail(e.to_string()))?;
    let asset =
        release["assets"].as_array().into_iter().flatten().find(|a| a["name"] == name.as_str()).ok_or_else(|| fail(name.clone()))?;
    // It will run on another machine: without GitHub's checksum to compare with, it is not used.
    let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).ok_or_else(|| fail(format!("{name}: no checksum")))?;
    let url = asset["browser_download_url"].as_str().unwrap_or_default();
    let mut bytes = Vec::new();
    crate::update::agent()
        .get(url)
        .header("User-Agent", &ua)
        .call()
        .map_err(|e| fail(e.to_string()))?
        .body_mut()
        .as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| fail(e.to_string()))?;
    use sha2::Digest;
    let sum: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if sum != digest.to_lowercase() {
        return Err(fail(format!("{name}: the download doesn't match the release")));
    }
    if let Some(dir) = kept.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        std::fs::write(&kept, &bytes).ok();
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_what_is_added() {
        let taken = [Machine { name: "vps".into(), target: "me@vps".into() }];
        assert!(check("box", "me@box", &taken).is_ok());
        assert!(check("box", "box-alias", &taken).is_ok());
        assert!(check("box", "me@10.0.0.2", &taken).is_ok());
        assert!(check("vps", "me@other", &taken).is_err(), "a name already taken");
        assert!(check(" ", "me@box", &taken).is_err());
        assert!(check("box", "", &taken).is_err());
        assert!(check("box", "-oProxyCommand=touch /tmp/x", &taken).is_err(), "an option, not a host");
        assert!(check("box", "me@box; rm -rf ~", &taken).is_err());
        assert!(check("box", "me@box$(id)", &taken).is_err());
    }

    #[test]
    fn the_remote_command_is_one_quoted_argument() {
        let c = remote_command("fr");
        assert!(c.starts_with("sh -c '") && c.ends_with('\'') && c.matches('\'').count() == 2, "{c}");
        assert!(c.contains(&format!("agent-{VERSION}")) && c.contains("serve --lang fr"), "{c}");
    }
}
