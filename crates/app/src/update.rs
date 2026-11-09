//! Updates: a thread asks GitHub for the latest release now and then; when it is newer, it downloads the installer
//! for this machine in the background and checks its SHA-256 against the one GitHub publishes. The bar then shows a
//! green arrow; a click installs it and starts the app again. Only an installed app updates (an `.app` bundle on
//! macOS, the install folder on Windows), never one run from a source checkout.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::watch;

/// What the bar shows, for the checking thread and a check asked from a menu.
static STATE: OnceLock<watch::Sender<State>> = OnceLock::new();
/// One check at a time: the thread's and a menu's would download the same file.
static CHECKING: Mutex<()> = Mutex::new(());

pub const REPO: &str = "flav-code/asterism";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// After launch, the first check waits a little: the history is being read then.
const FIRST_CHECK: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Up to date, not checked yet, or this copy can't update itself.
    Idle,
    Downloading {
        version: String,
        done: f32,
    },
    Ready {
        version: String,
        file: PathBuf,
    },
}

/// What this machine downloads: the end of the release asset's name.
fn asset_suffix() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("-macos-arm64.dmg")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("-macos-x64.dmg")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("-windows-x64-setup.exe")
    } else {
        None
    }
}

/// Where this copy is installed: the `.app` bundle on macOS, the install folder on Windows. None when it runs from a
/// build folder (then it never updates itself).
pub fn installed_at() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(target_os = "macos") {
        let bundle = exe.parent()?.parent()?.parent()?;
        (bundle.extension().is_some_and(|e| e == "app")).then(|| bundle.to_path_buf())
    } else if cfg!(windows) {
        let dir = exe.parent()?;
        dir.join("uninstall.exe").is_file().then(|| dir.to_path_buf())
    } else {
        None
    }
}

/// "0.10.2" > "0.9.7": numbers compared part by part.
pub fn newer(remote: &str, local: &str) -> bool {
    let parts = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(remote) > parts(local)
}

/// Starts the checking thread; the returned channel says what the bar should show.
pub fn start() -> watch::Receiver<State> {
    let tx = STATE.get_or_init(|| watch::channel(State::Idle).0);
    let rx = tx.subscribe();
    // What an earlier update left behind (its download, the copied bundle) goes; a pending one downloads again.
    std::fs::remove_dir_all(std::env::temp_dir().join("asterism-update")).ok();
    if installed_at().is_none() || std::env::var("ASTERISM_NO_UPDATES").is_ok() {
        return rx;
    }
    std::thread::Builder::new()
        .name("updates".into())
        .spawn(move || {
            // ASTERISM_UPDATE_URL (a test release) checks right away.
            std::thread::sleep(if std::env::var("ASTERISM_UPDATE_URL").is_ok() { Duration::from_secs(2) } else { FIRST_CHECK });
            loop {
                if let Err(e) = check_once(tx) {
                    eprintln!("update check: {e}");
                }
                if matches!(*tx.borrow(), State::Ready { .. }) || tx.is_closed() {
                    break; // one update waits for its click; the next check is after a restart
                }
                std::thread::sleep(CHECK_EVERY);
            }
        })
        .ok();
    rx
}

pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(600))).build().into()
}

/// What a check asked from a menu found.
pub enum Checked {
    UpToDate,
    /// This version is downloaded: the green arrow installs it.
    Ready(String),
    /// Scoop installed it, and updates it.
    Scoop,
    /// It runs from a build folder.
    NotInstalled,
    /// ASTERISM_NO_UPDATES is set.
    Off,
    Failed(String),
}

/// Checks right now (and downloads a newer version), whatever the thread's schedule. It blocks until done.
pub fn check_now() -> Checked {
    if std::env::var("ASTERISM_NO_UPDATES").is_ok() {
        return Checked::Off;
    }
    let (Some(_), Some(tx)) = (installed_at(), STATE.get()) else {
        let exe = std::env::current_exe().unwrap_or_default().to_string_lossy().to_lowercase().replace('\\', "/");
        return if exe.contains("/scoop/apps/") { Checked::Scoop } else { Checked::NotInstalled };
    };
    if let Err(e) = check_once(tx) {
        return Checked::Failed(e);
    }
    match &*tx.borrow() {
        State::Ready { version, .. } | State::Downloading { version, .. } => Checked::Ready(version.clone()),
        State::Idle => Checked::UpToDate,
    }
}

fn check_once(tx: &watch::Sender<State>) -> Result<(), String> {
    let _one = CHECKING.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(*tx.borrow(), State::Ready { .. }) {
        return Ok(()); // already downloaded: it waits for its click
    }
    check(tx)
}

fn check(tx: &watch::Sender<State>) -> Result<(), String> {
    let Some(suffix) = asset_suffix() else { return Ok(()) };
    let api = std::env::var("ASTERISM_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let ua = format!("Asterism/{}", env!("CARGO_PKG_VERSION"));
    let release: serde_json::Value = agent()
        .get(&api)
        .header("User-Agent", &ua)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    if !newer(tag, env!("CARGO_PKG_VERSION")) {
        return Ok(());
    }
    let version = tag.trim_start_matches('v').to_string();
    let Some(asset) = release["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
    else {
        return Ok(()); // this release has nothing for this machine
    };
    let (name, url) = (asset["name"].as_str().unwrap_or_default(), asset["browser_download_url"].as_str().unwrap_or_default());
    let size = asset["size"].as_u64().unwrap_or(0);
    let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_lowercase);
    let dir = std::env::temp_dir().join("asterism-update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(name);

    tx.send_replace(State::Downloading { version: version.clone(), done: 0.0 });
    let mut res = agent().get(url).header("User-Agent", &ua).call().map_err(|e| e.to_string())?;
    let total = res.body().content_length().unwrap_or(size).max(1);
    let mut reader = res.body_mut().as_reader();
    let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let (mut buf, mut got) = (vec![0u8; 256 * 1024], 0u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hash.update(&buf[..n]);
        got += n as u64;
        tx.send_replace(State::Downloading { version: version.clone(), done: (got as f32 / total as f32).min(1.0) });
    }
    out.flush().map_err(|e| e.to_string())?;
    let sum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    // GitHub publishes each asset's SHA-256: a file that doesn't match is never installed.
    if digest.as_deref().is_some_and(|d| d != sum) || (size > 0 && got != size) {
        std::fs::remove_file(&file).ok();
        tx.send_replace(State::Idle);
        return Err(format!("{name}: the download doesn't match the release"));
    }
    tx.send_replace(State::Ready { version, file });
    Ok(())
}

/// Installs the downloaded update and starts the new version; the caller quits the app right after.
pub fn apply(file: &Path) -> Result<(), String> {
    let at = installed_at().ok_or("not an installed app")?;
    if cfg!(target_os = "macos") {
        // Copy the new bundle out of the disk image, then a small script waits for this app to quit, swaps the
        // bundles and opens the new one.
        let work = file.with_extension("work");
        std::fs::remove_dir_all(&work).ok();
        let mount = work.join("mnt");
        std::fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .status()
                .map_err(|e| e.to_string())
                .and_then(|s| if s.success() { Ok(()) } else { Err(format!("{cmd} failed")) })
        };
        let m = mount.to_string_lossy().to_string();
        run("hdiutil", &["attach", "-nobrowse", "-readonly", "-mountpoint", &m, &file.to_string_lossy()])?;
        let new = work.join("Asterism.app");
        let copied = run("ditto", &[&format!("{m}/Asterism.app"), &new.to_string_lossy()]);
        run("hdiutil", &["detach", &m, "-quiet"]).ok();
        copied?;
        let script = format!(
            "while kill -0 {pid} 2>/dev/null; do sleep 0.2; done; rm -rf \"{old}\" && mv \"{new}\" \"{old}\" && open \"{old}\"; rm -rf \"{work}\"",
            pid = std::process::id(),
            old = at.display(),
            new = new.display(),
            work = work.display(),
        );
        std::process::Command::new("/bin/sh").args(["-c", &script]).spawn().map_err(|e| e.to_string())?;
    } else {
        // The installer waits a moment, installs over this copy silently, then starts Asterism.
        std::process::Command::new(file).arg("/S").spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions() {
        assert!(newer("v0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.7"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("v0.1.0", "0.2.0"));
    }
}
