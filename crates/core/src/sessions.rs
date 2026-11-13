//! Claude Code sessions, from the files Claude Code and its desktop app keep on disk.
//! These formats are internal and undocumented: read only, unknown fields ignored, missing files tolerated.
use crate::desktop::DesktopSession;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};

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

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn dir_entries(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path).into_iter().flatten().flatten().map(|e| e.path())
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The project a folder belongs to: its name, or the repository's for a worktree under `.claude/worktrees`.
#[must_use]
pub fn project_of(cwd: &str) -> String {
    let base = cwd.split("/.claude/worktrees/").next().unwrap_or(cwd).trim_end_matches(['/', '\\']);
    base.rsplit(['/', '\\']).next().unwrap_or(base).to_string()
}

const RECENT_MS: u64 = 2 * 86_400_000;

/// Reads the live-session registry on every sample and joins it to the desktop sessions; transcripts are found once.
#[derive(Default)]
pub struct SessionReader {
    transcripts: HashMap<String, PathBuf>,
}

impl SessionReader {
    /// Every session that may be running: the live registry's, then desktop sessions active in the last two days
    /// that it doesn't list (their process can still be found by its folder and start time).
    pub fn read(&mut self, home: &Path, desktop: &[DesktopSession], now_ms: u64) -> Vec<SessionMeta> {
        let registry: Vec<Value> = dir_entries(&home.join(".claude/sessions")).filter_map(|f| read_json(&f)).collect();
        let mut out = merge(desktop, &registry, now_ms);
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

/// Registry entries with their desktop session, then the recent desktop sessions the registry doesn't list.
fn merge(desktop: &[DesktopSession], registry: &[Value], now_ms: u64) -> Vec<SessionMeta> {
    let by_host = |id: &str| desktop.iter().find(|d| d.id == id);
    let by_cli = |id: &str| desktop.iter().find(|d| d.cli.as_deref() == Some(id));
    let mut out: Vec<SessionMeta> = Vec::new();

    for r in registry {
        let (Some(id), Some(pid)) = (str_of(r, "sessionId"), r.get("pid").and_then(Value::as_u64)) else { continue };
        let host_id = str_of(r, "hostSessionId");
        let d = host_id.as_deref().and_then(by_host).or_else(|| by_cli(&id));
        let mut s = meta(d, Some(r));
        s.id = id;
        s.pid = u32::try_from(pid).ok();
        s.host_id = host_id.or_else(|| d.map(|d| d.id.clone()));
        s.kind =
            if s.host_id.is_some() || str_of(r, "entrypoint").is_some_and(|e| e.contains("desktop")) { "desktop" } else { "cli" }.into();
        out.push(s);
    }

    for d in desktop {
        let Some(id) = d.cli.clone() else { continue };
        if now_ms.saturating_sub(d.last_activity_at) >= RECENT_MS || d.archived || out.iter().any(|s| s.id == id) {
            continue;
        }
        let mut s = meta(Some(d), None);
        s.id = id;
        s.host_id = Some(d.id.clone());
        s.kind = "desktop".into();
        out.push(s);
    }
    out
}

/// One session from its desktop file and its registry entry, either of which may be missing.
fn meta(d: Option<&DesktopSession>, r: Option<&Value>) -> SessionMeta {
    let cwd = r.and_then(|r| str_of(r, "cwd")).or_else(|| d.and_then(|d| d.cwd.clone()));
    let origin = d.and_then(|d| d.origin_cwd.clone()).or_else(|| cwd.clone());
    let dirs = d.map(|d| d.worktree.iter().chain(&d.written).chain(&d.folder_worktrees).cloned().collect()).unwrap_or_default();
    let title = r.and_then(|r| str_of(r, "name")).or_else(|| d.map(|d| d.title.clone()).filter(|t| !t.is_empty()));
    let project = origin.as_deref().map(project_of).unwrap_or_default();
    SessionMeta {
        title: title.unwrap_or_else(|| format!("claude · {project}")),
        project,
        cwd,
        status: r.and_then(|r| str_of(r, "status")),
        started_at: r.and_then(|r| r.get("startedAt")).and_then(Value::as_u64).or_else(|| d.map(|d| d.created_at).filter(|t| *t > 0)),
        dirs,
        visited: d.map(|d| d.shell_folders.clone()).unwrap_or_default(),
        ..Default::default()
    }
}

/// What the end of a transcript tells about now.
#[derive(Clone, Debug, Default)]
pub struct Tail {
    /// The latest tool call without a result.
    pub now: Option<Activity>,
    /// What each Bash call said it was for: its command → its description. A command sent to the background has
    /// its result at once and runs on, so this is the only place that says what it is.
    pub said: HashMap<String, String>,
}

impl Tail {
    /// Takes from the reading before what the commands still running said: the transcript has moved on past their
    /// calls (one picture is bigger than all this reads), and nothing else says what they are.
    pub fn keep_running(&mut self, before: Tail, running: &HashSet<String>) {
        for (command, what) in before.said {
            if running.contains(&command) {
                self.said.entry(command).or_insert(what);
            }
        }
    }
}

/// The end of a transcript (one JSON event per line).
#[must_use]
pub fn tail(transcript: &Path) -> Tail {
    read_tail(transcript).unwrap_or_default()
}

fn read_tail(transcript: &Path) -> Option<Tail> {
    const TAIL: u64 = 512 * 1024;
    let mut f = fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // The first line is cut when the tail starts mid-file.
    let lines = text.lines().skip(usize::from(len > TAIL));
    Some(tail_in(lines))
}

/// Same, over transcript lines.
pub fn tail_in<'a>(lines: impl Iterator<Item = &'a str>) -> Tail {
    let mut pending: Vec<(String, Activity)> = Vec::new();
    let mut said = HashMap::new();
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
                    if let (Some(c), Some(_)) = (&command, str_of(&input, "description")) {
                        said.insert(c.trim().to_string(), detail.clone());
                    }
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
    Tail { now: pending.pop().map(|(_, a)| a), said }
}

/// "2026-11-07T20:15:03.123Z" → ms since the epoch (UTC only, which is what transcripts hold).
#[must_use]
pub fn iso_ms(s: &str) -> Option<u64> {
    let n = |r: Range<usize>| s.get(r)?.parse::<i64>().ok();
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
        let tail = tail_in(lines.into_iter());
        // a call that ended keeps what it said it was for: its command may still run in the background
        assert_eq!(tail.said.get("npx vitest run").map(String::as_str), Some("Run the tests"));
        // read again once the transcript has moved on: only a command still running keeps what it said
        let mut later = tail_in(lines[..2].iter().copied());
        later.keep_running(tail.clone(), &HashSet::from(["npx vitest run".to_string()]));
        assert_eq!(later.said.len(), 1);
        later = tail_in(lines[..2].iter().copied());
        later.keep_running(tail.clone(), &HashSet::new());
        assert!(later.said.is_empty());
        let a = tail.now.unwrap();
        assert_eq!((a.tool.as_str(), a.detail.as_str(), a.command.as_deref()), ("Bash", "Run the tests", Some("npx vitest run")));
        assert_eq!(a.since, iso_ms("2026-11-07T20:00:05Z").unwrap());
        assert!(tail_in(lines[..2].iter().copied()).now.is_none());
        // a copy of an ended call in another kind of line (no timestamp) is not a new call
        let copy = r#"{"type":"api-request-blob","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#;
        assert!(tail_in([lines[0], lines[1], copy].into_iter()).now.is_none());
        let mcp = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"mcp__browser__navigate","input":{"url":"https://example.com"}}]}}"#;
        assert_eq!(tail_in([mcp].into_iter()).now.unwrap().tool, "navigate");
    }

    #[test]
    fn projects() {
        assert_eq!(project_of("/home/me/src/app/.claude/worktrees/fix-x"), "app");
        assert_eq!(project_of("/home/me/src/app/"), "app");
    }
}
