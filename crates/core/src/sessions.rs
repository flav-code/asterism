//! Claude Code sessions, from the files Claude Code and its desktop app keep on disk.
//! These formats are internal and undocumented: read only, unknown fields ignored, missing files tolerated.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    /// Claude Code's session id, which names the transcript.
    pub id: String,
    /// The desktop app's own id for the session, when it runs there.
    pub host_id: Option<String>,
    /// The `claude` process, from the live-session registry (the file may outlive it).
    pub pid: Option<u32>,
    pub title: String,
    pub project: String,
    pub cwd: Option<String>,
    /// "desktop" or "cli".
    pub kind: String,
    /// "busy" or "idle", when the registry says.
    pub status: Option<String>,
    /// When the session started, in ms since the epoch.
    pub started_at: Option<u64>,
    /// Folders it works in besides its cwd: its worktree, the repositories it wrote to.
    pub dirs: Vec<String>,
    /// Folders its shell only went through.
    pub visited: Vec<String>,
    pub transcript: Option<PathBuf>,
}

/// What a session is doing: its latest tool call that has no result yet.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Activity {
    pub tool: String,
    /// The call's description, command, file or query: whatever says best what it does.
    pub detail: String,
    /// The shell command, for a Bash call: used to find the process that runs it.
    pub command: Option<String>,
    /// When the call started, in ms since the epoch.
    pub since: u64,
}

const DESKTOP_APP_PREFIX: &str = "Claude";

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

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn dir_entries(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path())
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key).and_then(Value::as_array).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

/// The project a folder belongs to: its name, or the repository's for a worktree under `.claude/worktrees`.
pub fn project_of(cwd: &str) -> String {
    let base = cwd.split("/.claude/worktrees/").next().unwrap_or(cwd).trim_end_matches(['/', '\\']);
    base.rsplit(['/', '\\']).next().unwrap_or(base).to_string()
}

/// Desktop app session files: every `local_<session id>.json` under each app's `claude-code-sessions` folder.
fn desktop_files(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for data in app_data_dirs(home) {
        for app in dir_entries(&data) {
            let is_app = app.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(DESKTOP_APP_PREFIX));
            if !is_app {
                continue;
            }
            for a in dir_entries(&app.join("claude-code-sessions")) {
                for b in dir_entries(&a) {
                    for f in dir_entries(&b) {
                        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                        if name.starts_with("local_") && name.ends_with(".json") {
                            out.push(f);
                        }
                    }
                }
            }
        }
    }
    out
}

const RECENT_MS: u64 = 2 * 86_400_000;
/// What is kept of a desktop session file: they are large (they hold turn reports), and only these fields matter.
const DESKTOP_KEYS: &[&str] = &[
    "sessionId",
    "cliSessionId",
    "title",
    "cwd",
    "originCwd",
    "isArchived",
    "lastActivityAt",
    "createdAt",
    "worktreePath",
    "writtenRepositories",
    "folderWorktrees",
    "shellFolders",
];

/// Reads the sessions on every sample without decoding everything each time: a desktop file is decoded again only
/// when it changes, files idle for two days are skipped unless their session runs, transcripts are found once.
#[derive(Default)]
pub struct SessionReader {
    desktop: HashMap<PathBuf, (Option<SystemTime>, u64, Value)>,
    transcripts: HashMap<String, PathBuf>,
}

impl SessionReader {
    /// Every session that may be running: the live registry's, then recent desktop sessions it doesn't list
    /// (their process can still be found by its folder and start time).
    pub fn read(&mut self, home: &Path, now_ms: u64) -> Vec<SessionMeta> {
        let registry: Vec<Value> = dir_entries(&home.join(".claude/sessions")).filter_map(|f| read_json(&f)).collect();
        let live: HashSet<String> = registry.iter().filter_map(|r| str_of(r, "hostSessionId")).collect();
        let now = SystemTime::now();
        let mut seen = HashSet::new();
        for f in desktop_files(home) {
            let Ok(md) = fs::metadata(&f) else { continue };
            let modified = md.modified().ok();
            let recent = modified.and_then(|m| now.duration_since(m).ok()).is_none_or(|age| age.as_millis() < u128::from(RECENT_MS));
            let name = f.file_stem().and_then(|n| n.to_str()).unwrap_or_default();
            if !recent && !live.contains(name) {
                continue;
            }
            let unchanged = matches!(self.desktop.get(&f), Some((m, len, _)) if *m == modified && *len == md.len());
            if !unchanged {
                let Some(v) = read_json(&f) else { continue };
                let small = DESKTOP_KEYS.iter().filter_map(|k| Some((k.to_string(), v.get(*k)?.clone()))).collect();
                self.desktop.insert(f.clone(), (modified, md.len(), Value::Object(small)));
            }
            seen.insert(f);
        }
        self.desktop.retain(|f, _| seen.contains(f));
        let desktop: Vec<&Value> = self.desktop.values().map(|(_, _, v)| v).collect();
        let mut out = merge(&desktop, &registry, now_ms);

        let folders: Vec<PathBuf> = dir_entries(&home.join(".claude/projects")).collect();
        self.transcripts.retain(|_, p| p.is_file());
        for s in &mut out {
            if !self.transcripts.contains_key(&s.id)
                && let Some(p) = folders.iter().map(|f| f.join(format!("{}.jsonl", s.id))).find(|p| p.is_file())
            {
                self.transcripts.insert(s.id.clone(), p);
            }
            s.transcript = self.transcripts.get(&s.id).cloned();
        }
        out
    }
}

/// Registry entries with their desktop file, then the recent desktop sessions the registry doesn't list.
fn merge(desktop: &[&Value], registry: &[Value], now_ms: u64) -> Vec<SessionMeta> {
    let by_host = |id: &str| desktop.iter().copied().find(|d| d.get("sessionId").and_then(Value::as_str) == Some(id));
    let by_cli = |id: &str| desktop.iter().copied().find(|d| d.get("cliSessionId").and_then(Value::as_str) == Some(id));
    let mut out: Vec<SessionMeta> = Vec::new();

    for r in registry {
        let (Some(id), Some(pid)) = (str_of(r, "sessionId"), r.get("pid").and_then(Value::as_u64)) else { continue };
        let host_id = str_of(r, "hostSessionId");
        let d = host_id.as_deref().and_then(by_host).or_else(|| by_cli(&id));
        let mut s = meta(d, Some(r));
        s.id = id;
        s.pid = u32::try_from(pid).ok();
        s.host_id = host_id.or_else(|| d.and_then(|d| str_of(d, "sessionId")));
        s.kind =
            if s.host_id.is_some() || str_of(r, "entrypoint").is_some_and(|e| e.contains("desktop")) { "desktop" } else { "cli" }.into();
        out.push(s);
    }

    for d in desktop.iter().copied() {
        let Some(id) = str_of(d, "cliSessionId") else { continue };
        let recent = d.get("lastActivityAt").and_then(Value::as_u64).is_some_and(|t| now_ms.saturating_sub(t) < RECENT_MS);
        let archived = d.get("isArchived").and_then(Value::as_bool).unwrap_or(false);
        if !recent || archived || out.iter().any(|s| s.id == id) {
            continue;
        }
        let mut s = meta(Some(d), None);
        s.id = id;
        s.host_id = str_of(d, "sessionId");
        s.kind = "desktop".into();
        out.push(s);
    }
    out
}

/// One session from its desktop file and its registry entry, either of which may be missing.
fn meta(d: Option<&Value>, r: Option<&Value>) -> SessionMeta {
    let get = |key: &str| d.and_then(|d| str_of(d, key));
    let cwd = r.and_then(|r| str_of(r, "cwd")).or_else(|| get("cwd"));
    let origin = get("originCwd").or_else(|| cwd.clone());
    let mut dirs: Vec<String> = Vec::new();
    if let Some(d) = d {
        dirs.extend(str_of(d, "worktreePath"));
        dirs.extend(strings(d, "writtenRepositories"));
        for w in d.get("folderWorktrees").and_then(Value::as_array).into_iter().flatten() {
            if !w.get("removed").and_then(Value::as_bool).unwrap_or(false) {
                dirs.extend(str_of(w, "worktreePath"));
            }
        }
    }
    let title = r.and_then(|r| str_of(r, "name")).or_else(|| get("title"));
    let project = origin.as_deref().map(project_of).unwrap_or_default();
    SessionMeta {
        title: title.unwrap_or_else(|| format!("claude · {project}")),
        project,
        cwd,
        status: r.and_then(|r| str_of(r, "status")),
        started_at: r.and_then(|r| r.get("startedAt")).or_else(|| d.and_then(|d| d.get("createdAt"))).and_then(Value::as_u64),
        dirs,
        visited: d.map(|d| strings(d, "shellFolders")).unwrap_or_default(),
        ..Default::default()
    }
}

/// The latest tool call without a result, from the end of a transcript (one JSON event per line).
pub fn current_activity(transcript: &Path) -> Option<Activity> {
    const TAIL: u64 = 512 * 1024;
    let mut f = fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // The first line is cut when the tail starts mid-file.
    let lines = text.lines().skip(usize::from(len > TAIL));
    activity_in(lines)
}

/// Same, over transcript lines.
pub fn activity_in<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Activity> {
    let mut pending: Vec<(String, Activity)> = Vec::new();
    for line in lines {
        let calls = line.contains("\"tool_use\"");
        if !calls && !line.contains("\"tool_result\"") {
            continue;
        }
        let Ok(j) = serde_json::from_str::<Value>(line) else { continue };
        // Only the conversation itself: other lines (request copies, progress) repeat calls that already ended.
        let kind = j.get("type").and_then(Value::as_str);
        let since = j.get("timestamp").and_then(Value::as_str).and_then(iso_ms).unwrap_or(0);
        for b in j.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_use") if kind == Some("assistant") => {
                    let (Some(id), Some(name)) = (str_of(b, "id"), str_of(b, "name")) else { continue };
                    let input = b.get("input").cloned().unwrap_or(Value::Null);
                    let detail = ["description", "command", "file_path", "pattern", "url", "query", "prompt", "title"]
                        .iter()
                        .find_map(|k| str_of(&input, k))
                        .map(|s| s.lines().next().unwrap_or_default().to_string())
                        .unwrap_or_default();
                    let tool = name.rsplit("__").next().unwrap_or(&name).to_string();
                    let command = (tool == "Bash").then(|| str_of(&input, "command")).flatten();
                    pending.push((id, Activity { tool, detail, command, since }));
                }
                Some("tool_result") if kind == Some("user") => {
                    if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                        pending.retain(|(p, _)| p != id);
                    }
                }
                _ => {}
            }
        }
    }
    pending.pop().map(|(_, a)| a)
}

/// "2026-11-07T20:15:03.123Z" → ms since the epoch (UTC only, which is what transcripts hold).
pub fn iso_ms(s: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let ms = s.get(19..).filter(|r| r.starts_with('.')).map_or(0, |r| {
        let digits: String = r[1..].chars().take_while(char::is_ascii_digit).take(3).collect();
        format!("{digits:0<3}").parse::<i64>().unwrap_or(0)
    });
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_ms("2026-11-07T20:15:03.123Z"), Some(1_794_082_503_123));
        assert_eq!(iso_ms("2024-02-29T12:00:00.5Z"), Some(1_709_208_000_500));
    }

    #[test]
    fn running_tool_is_the_last_without_result() {
        let lines = [
            r#"{"type":"assistant","timestamp":"2026-11-07T20:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/src/a.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-07T20:00:05Z","message":{"content":[{"type":"text","text":"running tests"},{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"npx vitest run","description":"Run the tests"}}]}}"#,
        ];
        let a = activity_in(lines.into_iter()).unwrap();
        assert_eq!((a.tool.as_str(), a.detail.as_str(), a.command.as_deref()), ("Bash", "Run the tests", Some("npx vitest run")));
        assert_eq!(a.since, iso_ms("2026-11-07T20:00:05Z").unwrap());
        assert!(activity_in(lines[..2].iter().copied()).is_none());
        // a copy of an ended call in another kind of line (no timestamp) is not a new call
        let copy = r#"{"type":"api-request-blob","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#;
        assert!(activity_in([lines[0], lines[1], copy].into_iter()).is_none());
        let mcp = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"mcp__browser__navigate","input":{"url":"https://example.com"}}]}}"#;
        assert_eq!(activity_in([mcp].into_iter()).unwrap().tool, "navigate");
    }

    #[test]
    fn projects() {
        assert_eq!(project_of("/home/me/src/app/.claude/worktrees/fix-x"), "app");
        assert_eq!(project_of("/home/me/src/app/"), "app");
    }
}
