//! The desktop app's session files (`local_<id>.json`, one per session, in each app profile). They are large
//! (they hold turn reports), so each is decoded once into the few fields used here, and again only when it changes.
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const DESKTOP_APP_PREFIX: &str = "Claude";

#[derive(Clone, Debug, Default)]
pub struct Pr {
    pub n: u64,
    pub url: String,
    pub state: String,
    pub repo: String,
}

/// A session that reported back to this one.
#[derive(Clone, Debug, Default)]
pub struct Note {
    pub from: String,
    pub key: String,
    pub at: u64,
    pub title: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct DesktopSession {
    pub id: String,
    /// The app profile folder it lives in ("Claude", "Claude-3"…).
    pub profile: String,
    pub cli: Option<String>,
    pub prior_cli: Vec<String>,
    pub bridges: Vec<String>,
    pub title: String,
    pub previous_titles: Vec<String>,
    pub cwd: Option<String>,
    pub origin_cwd: Option<String>,
    pub created_at: u64,
    pub last_activity_at: u64,
    pub archived: bool,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub turns: u64,
    pub scheduled: bool,
    pub prs: Vec<Pr>,
    pub spawned_from: Option<String>,
    pub forked_from: Option<String>,
    pub notes: Vec<Note>,
    pub worktree: Option<String>,
    pub written: Vec<String>,
    pub folder_worktrees: Vec<String>,
    pub shell_folders: Vec<String>,
    /// The machine it runs on when the app reaches it over SSH: the name it was given, else its host (without the user).
    pub ssh: Option<String>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}
fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}
fn list<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}
fn strings(v: &Value, k: &str) -> Vec<String> {
    list(v, k).filter_map(|x| x.as_str().map(str::to_string)).collect()
}
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The fields used here, from one decoded file. Unknown fields are ignored and odd ones read as absent.
pub fn parse(v: &Value, profile: &str) -> Option<DesktopSession> {
    let id = s(v, "sessionId")?;
    let notes = list(v, "sideSessionNotes")
        .chain(list(v, "queuedSideSessionNotes"))
        .filter_map(|n| {
            Some(Note {
                from: s(n, "sessionId")?,
                key: s(n, "messageUuid").unwrap_or_else(|| u(n, "at").to_string()),
                at: u(n, "at"),
                title: clip(&s(n, "title").unwrap_or_default(), 140),
                text: clip(&s(n, "report").unwrap_or_default(), 320),
            })
        })
        .collect();
    Some(DesktopSession {
        id,
        profile: profile.to_string(),
        cli: s(v, "cliSessionId"),
        prior_cli: strings(v, "priorCliSessionIds"),
        bridges: list(v, "bridgeSessionIds").filter_map(|b| b.get("id").unwrap_or(b).as_str().map(str::to_string)).collect(),
        title: s(v, "title").unwrap_or_default(),
        previous_titles: list(v, "previousTitles")
            .filter_map(|t| t.as_str().or_else(|| t.get("title")?.as_str()).map(str::to_string))
            .collect(),
        cwd: s(v, "cwd"),
        origin_cwd: s(v, "originCwd"),
        created_at: u(v, "createdAt"),
        last_activity_at: u(v, "lastActivityAt"),
        archived: v.get("isArchived").and_then(Value::as_bool).unwrap_or(false),
        model: s(v, "model"),
        branch: s(v, "branch").or_else(|| s(v, "worktreeName")),
        turns: u(v, "completedTurns"),
        scheduled: v.get("scheduledTaskId").is_some_and(|t| !t.is_null()),
        prs: list(v, "prs")
            .map(|p| Pr {
                n: u(p, "prNumber"),
                url: s(p, "url").unwrap_or_default(),
                state: s(p, "state").unwrap_or_default(),
                repo: s(p, "repo").unwrap_or_default(),
            })
            .collect(),
        spawned_from: v.pointer("/spawnedFrom/sessionId").and_then(Value::as_str).map(str::to_string),
        forked_from: s(v, "forkedFromSessionId"),
        notes,
        worktree: s(v, "worktreePath"),
        written: strings(v, "writtenRepositories"),
        folder_worktrees: list(v, "folderWorktrees")
            .filter(|w| !w.get("removed").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(|w| s(w, "worktreePath"))
            .collect(),
        shell_folders: strings(v, "shellFolders"),
        ssh: v
            .get("sshConfig")
            .and_then(|c| s(c, "name").or_else(|| s(c, "sshHost").map(|h| h.rsplit('@').next().unwrap_or(&h).to_string()))),
    })
}

/// Where desktop apps keep their data, per OS.
fn app_data_dirs(home: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support")]
    } else if cfg!(windows) {
        // shortcut: not checked on a real Windows yet; the desktop app may keep its sessions elsewhere.
        std::env::var_os("APPDATA").map(PathBuf::from).into_iter().collect()
    } else {
        vec![home.join(".config")]
    }
}

fn entries(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path())
}

/// Every `local_*.json` under each app profile's `claude-code-sessions` folder, with its profile name.
fn files(home: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for data in app_data_dirs(home) {
        for app in entries(&data) {
            let Some(profile) = app.file_name().and_then(|n| n.to_str()).filter(|n| n.starts_with(DESKTOP_APP_PREFIX)).map(str::to_string)
            else {
                continue;
            };
            for a in entries(&app.join("claude-code-sessions")) {
                for b in entries(&a) {
                    for f in entries(&b) {
                        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                        if name.starts_with("local_") && name.ends_with(".json") {
                            out.push((f, profile.clone()));
                        }
                    }
                }
            }
        }
    }
    out
}

#[derive(Default)]
pub struct DesktopStore {
    files: HashMap<PathBuf, Decoded>,
}

/// A session file as it was last decoded: read again only when its date or size changes.
struct Decoded {
    modified: Option<SystemTime>,
    size: u64,
    /// None for a file that holds no session.
    session: Option<DesktopSession>,
}

impl DesktopStore {
    /// Brings every file up to date (decoding only the changed ones) and returns all sessions.
    pub fn refresh(&mut self, home: &Path) -> Vec<DesktopSession> {
        let mut seen = HashSet::new();
        for (f, profile) in files(home) {
            let Ok(md) = fs::metadata(&f) else { continue };
            let modified = md.modified().ok();
            let unchanged = self.files.get(&f).is_some_and(|d| d.modified == modified && d.size == md.len());
            if !unchanged {
                let parsed = fs::read(&f).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| parse(&v, &profile));
                self.files.insert(f.clone(), Decoded { modified, size: md.len(), session: parsed });
            }
            seen.insert(f);
        }
        self.files.retain(|f, _| seen.contains(f));
        self.files.values().filter_map(|d| d.session.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_ssh_machine() {
        let ssh = |c: Value| parse(&serde_json::json!({ "sessionId": "local_x", "sshConfig": c }), "Claude").and_then(|d| d.ssh);
        assert_eq!(ssh(serde_json::json!({ "sshHost": "me@10.0.0.2" })).as_deref(), Some("10.0.0.2"));
        assert_eq!(ssh(serde_json::json!({ "sshHost": "box" })).as_deref(), Some("box"));
        assert_eq!(ssh(serde_json::json!({ "sshHost": "me@10.0.0.2", "name": "build box" })).as_deref(), Some("build box"));
        assert_eq!(ssh(Value::Null), None);
    }
}
