//! Transcripts, read as they grow: the messages sessions send each other (SendMessage and its result), each
//! session's latest move, its custom title and its token usage. Each file is read from where the last read stopped.
use crate::usage::{FileTotals, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// A SendMessage call, and what its result said.
#[derive(Clone, Debug, Serialize)]
pub struct Sent {
    pub id: String,
    pub ts: u64,
    pub to: String,
    pub summary: String,
    pub text: String,
    /// None while no result came back.
    pub ok: Option<bool>,
    pub note: String,
}

/// The session's latest move: its last tool call, or "reply" for a text answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Move {
    pub tool: String,
    pub detail: String,
    pub ts: u64,
}

#[derive(Default)]
pub struct Transcript {
    offset: u64,
    carry: Vec<u8>,
    pending: HashMap<String, usize>,
    pub sent: Vec<Sent>,
    pub last: Option<Move>,
    pub title: Option<String>,
    pub tokens: FileTotals,
    /// A subagent's transcript.
    pub agent: bool,
    /// From its first prompt: the folder, what started it ("cli", "claude-desktop"…), when, and that prompt's first
    /// line (a terminal session's title when it has none).
    pub cwd: Option<String>,
    pub entrypoint: Option<String>,
    pub started: u64,
    pub prompt: Option<String>,
    /// Last change, in ms since the epoch.
    pub modified: u64,
    /// The file's size when last looked at.
    size: u64,
}

impl Transcript {
    /// Everything the file held when last looked at has been read.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.offset >= self.size
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One readable line from markdown: no emphasis, code ticks, heading marks or link targets.
pub fn plain(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut out = String::new();
    let mut rest = line.trim_start_matches('#').trim_start();
    while let Some(i) = rest.find("](") {
        let (head, tail) = rest.split_at(i);
        out.push_str(&head.replacen('[', "", 1));
        rest = tail[2..].split_once(')').map_or("", |(_, after)| after);
    }
    out.push_str(rest);
    clip(&out.replace("**", "").replace('`', ""), 90)
}

fn content(j: &Value) -> impl Iterator<Item = &Value> {
    j.pointer("/message/content").and_then(Value::as_array).into_iter().flatten()
}

fn handle(st: &mut Transcript, line: &str, usage: &mut Usage, now_ms: u64) {
    if st.prompt.is_none()
        && line.contains("\"type\":\"user\"")
        && let Ok(j) = serde_json::from_str::<Value>(line)
        && j.get("type").and_then(Value::as_str) == Some("user")
    {
        let s = |k: &str| j.get(k).and_then(Value::as_str).map(str::to_string);
        (st.cwd, st.entrypoint) = (st.cwd.take().or_else(|| s("cwd")), st.entrypoint.take().or_else(|| s("entrypoint")));
        if st.started == 0 {
            st.started = s("timestamp").as_deref().and_then(crate::sessions::iso_ms).unwrap_or(0);
        }
        let text = match j.pointer("/message/content") {
            Some(Value::String(t)) => Some(t.as_str()),
            Some(Value::Array(blocks)) => blocks.iter().find_map(|b| b.get("text").and_then(Value::as_str)),
            _ => None,
        };
        // Tags are the app's own lines (a command's output, a scheduled task), not what someone asked.
        st.prompt = text.filter(|t| !t.trim().is_empty() && !t.trim_start().starts_with('<')).map(plain);
    }
    if line.contains("\"type\":\"custom-title\"") {
        if let Ok(j) = serde_json::from_str::<Value>(line)
            && j.get("type").and_then(Value::as_str) == Some("custom-title")
            && let Some(t) = j.get("customTitle").and_then(Value::as_str)
        {
            st.title = Some(t.to_string());
        }
        return;
    }
    if line.contains("\"type\":\"assistant\"") {
        let Ok(j) = serde_json::from_str::<Value>(line) else { return };
        if j.get("type").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        if j.pointer("/message/usage").is_some() {
            usage.add(&mut st.tokens, &j, st.agent, now_ms);
        }
        let ts = j.get("timestamp").and_then(Value::as_str).and_then(crate::sessions::iso_ms).unwrap_or(0);
        let blocks: Vec<&Value> = content(&j).collect();
        if let Some(t) = blocks.iter().rev().find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")) {
            let name = t.get("name").and_then(Value::as_str).unwrap_or_default();
            let input = t.get("input").cloned().unwrap_or_default();
            let detail = ["description", "summary", "command", "file_path", "query", "url", "pattern", "prompt", "title"]
                .iter()
                .find_map(|k| input.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
                .map(plain)
                .unwrap_or_default();
            st.last = Some(Move { tool: name.rsplit("__").next().unwrap_or(name).to_string(), detail, ts });
        } else if let Some(text) = blocks.iter().find_map(|b| b.get("text").and_then(Value::as_str)) {
            st.last = Some(Move { tool: "reply".into(), detail: plain(text), ts });
        }
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("tool_use")
                && b.get("name").and_then(Value::as_str) == Some("SendMessage")
                && let (Some(id), Some(to)) = (b.get("id").and_then(Value::as_str), b.pointer("/input/to"))
            {
                let to = to.as_str().map_or_else(|| to.to_string(), str::to_string);
                let text = |k: &str| b.pointer(&format!("/input/{k}")).and_then(Value::as_str).unwrap_or_default().to_string();
                st.pending.insert(id.to_string(), st.sent.len());
                st.sent.push(Sent {
                    id: id.into(),
                    ts,
                    to,
                    summary: clip(&text("summary"), 140),
                    text: clip(&text("message"), 320),
                    ok: None,
                    note: String::new(),
                });
            }
        }
    } else if !st.pending.is_empty() && line.contains("\"tool_result\"") {
        let Ok(j) = serde_json::from_str::<Value>(line) else { return };
        for b in content(&j) {
            let Some(i) = b.get("tool_use_id").and_then(Value::as_str).and_then(|id| st.pending.remove(id)) else { continue };
            let raw = match b.get("content") {
                Some(Value::Array(parts)) => parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<String>(),
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let parsed = serde_json::from_str::<Value>(&raw).ok();
            let m = &mut st.sent[i];
            m.ok = Some(match &parsed {
                Some(r) => r.get("success").and_then(Value::as_bool) != Some(false),
                None => !b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            });
            m.note = clip(parsed.as_ref().and_then(|r| r.get("message")?.as_str()).unwrap_or(&raw), 220);
        }
    }
}

#[derive(Default)]
pub struct Transcripts {
    pub files: HashMap<PathBuf, Transcript>,
}

impl Transcripts {
    /// Bytes read so far, over every file.
    #[must_use]
    pub fn read_bytes(&self) -> u64 {
        self.files.values().map(|t| t.offset).sum()
    }

    /// Reads what was appended to `path` since the last call; None when the file is gone.
    pub fn scan(&mut self, path: &Path, usage: &mut Usage, now_ms: u64) -> Option<&Transcript> {
        let mut no_limit = u64::MAX;
        self.scan_within(path, usage, now_ms, &mut no_limit)
    }

    /// `scan`, reading at most `budget` bytes (taken from it): the rest waits for a later call.
    pub fn scan_within(&mut self, path: &Path, usage: &mut Usage, now_ms: u64, budget: &mut u64) -> Option<&Transcript> {
        let md = fs::metadata(path).ok()?;
        let len = md.len();
        let st = self
            .files
            .entry(path.to_path_buf())
            .or_insert_with(|| Transcript { agent: path.components().any(|c| c.as_os_str() == "subagents"), ..Default::default() });
        if len < st.offset {
            *st = Transcript { agent: st.agent, ..Default::default() }; // rewritten from scratch
        }
        st.modified = md.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
        st.size = len;
        if len > st.offset && *budget > 0 {
            let mut f = fs::File::open(path).ok()?;
            f.seek(SeekFrom::Start(st.offset)).ok()?;
            let mut chunk = vec![0; (len - st.offset).min(8 << 20).min(*budget) as usize];
            while st.offset < len && *budget > 0 {
                let want = (chunk.len() as u64).min(*budget) as usize;
                let read = f.read(&mut chunk[..want]).ok()?;
                if read == 0 {
                    break;
                }
                st.offset += read as u64;
                *budget -= read as u64;
                let mut data = std::mem::take(&mut st.carry);
                data.extend_from_slice(&chunk[..read]);
                let mut start = 0;
                while let Some(i) = data[start..].iter().position(|b| *b == b'\n') {
                    handle(st, &String::from_utf8_lossy(&data[start..start + i]), usage, now_ms);
                    start += i + 1;
                }
                st.carry = data[start..].to_vec();
            }
        }
        Some(st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_within_a_budget_then_finishes() {
        let dir = std::env::temp_dir().join(format!("mdb-budget-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let line = r#"{"type":"user","timestamp":"2026-10-31T01:00:00Z","message":{"content":"hi"}}"#;
        fs::write(&path, format!("{line}\n").repeat(100)).unwrap();
        let (mut t, mut usage) = (Transcripts::default(), Usage::default());
        let mut budget = 1000;
        assert!(!t.scan_within(&path, &mut usage, 0, &mut budget).unwrap().complete());
        assert_eq!(budget, 0, "the whole budget is used");
        assert!(t.scan(&path, &mut usage, 0).unwrap().complete());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_sent_message_and_its_result() {
        let mut st = Transcript::default();
        let mut usage = Usage::default();
        handle(
            &mut st,
            r#"{"type":"assistant","timestamp":"2026-10-31T01:00:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"SendMessage","input":{"to":"x","summary":"s","message":"m"}}]}}"#,
            &mut usage,
            0,
        );
        handle(
            &mut st,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"{\"success\":false,\"message\":\"nope\"}"}]}]}}"#,
            &mut usage,
            0,
        );
        assert_eq!((st.sent.len(), st.sent[0].ok, st.sent[0].note.as_str(), st.pending.len()), (1, Some(false), "nope", 0));
        assert_eq!(st.last.as_ref().map(|m| m.tool.as_str()), Some("SendMessage"));
    }

    #[test]
    fn readable_lines() {
        assert_eq!(plain("\n## **Done**: see [the PR](https://x/1) and `cargo test`\nmore"), "Done: see the PR and cargo test");
    }
}
