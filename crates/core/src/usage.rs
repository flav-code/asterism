//! Token usage read from transcripts: per day and model, per hour, per minute (the last two hours), tools,
//! the biggest reply. A reply spans several transcript lines and forks copy history, so everything is counted
//! once per message id (and tool calls once per tool_use id).
use chrono::{Local, TimeZone, Timelike};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Anthropic API prices in $ per million tokens: input, output, cache read. Cache writes cost 1.25× input
/// (5-minute TTL) or 2× (1-hour TTL). From the published price list of 2026-10-06.
pub const PRICES: &[(&str, f64, f64, f64)] = &[
    ("claude-fable-5-1", 10.0, 50.0, 0.25),
    ("claude-mythos-5-1", 10.0, 50.0, 0.25),
    ("claude-fable-5", 10.0, 50.0, 1.0),
    ("claude-opus-5-5", 4.0, 20.0, 0.2),
    ("claude-opus-5", 5.0, 25.0, 0.5),
    ("claude-opus-4-8", 5.0, 25.0, 0.5),
    ("claude-opus-4-7", 5.0, 25.0, 0.5),
    ("claude-opus-4-6", 5.0, 25.0, 0.5),
    ("claude-sonnet-5-5", 2.0, 10.0, 0.2),
    ("claude-sonnet-5", 2.0, 10.0, 0.2),
    ("claude-sonnet-4-6", 3.0, 15.0, 0.3),
    ("claude-haiku-5-5", 0.1, 0.5, 0.01),
    ("claude-haiku-4-5", 1.0, 5.0, 0.1),
];

/// "claude-opus-4-8[1m]" or "claude-haiku-4-5-20251001" → the priced id.
pub fn model_id(m: &str) -> &str {
    let m = m.split('[').next().unwrap_or(m);
    match m.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => m,
    }
}

pub fn family(m: &str) -> &'static str {
    match m {
        m if m.contains("fable") || m.contains("mythos") => "Fable",
        m if m.contains("opus") => "Opus",
        m if m.contains("sonnet") => "Sonnet",
        m if m.contains("haiku") => "Haiku",
        _ => "Other",
    }
}

fn n(u: &Value, k: &str) -> f64 {
    u.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

/// The API price of one reply's usage, None for a model without a known price.
pub fn cost(model: &str, u: &Value) -> Option<f64> {
    let id = model_id(model);
    let &(_, input, output, read) = PRICES.iter().find(|p| p.0 == id)?;
    let (i, o, cr, cw) =
        (n(u, "input_tokens"), n(u, "output_tokens"), n(u, "cache_read_input_tokens"), n(u, "cache_creation_input_tokens"));
    let cw1h = u.pointer("/cache_creation/ephemeral_1h_input_tokens").and_then(Value::as_f64).unwrap_or(0.0);
    let mut k = if u.get("speed").and_then(Value::as_str) == Some("fast") { 2.0 } else { 1.0 }; // fast mode costs twice
    if id == "claude-haiku-5-5" && i + cr + cw > 100e3 {
        k *= 5.0; // long-prompt Haiku 5.5 rate
    }
    Some(k * (i * input + o * output + cr * read + (cw - cw1h) * input * 1.25 + cw1h * input * 2.0) / 1e6)
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct Totals {
    pub i: u64,
    pub o: u64,
    pub cr: u64,
    pub cw: u64,
    /// Replies.
    pub n: u64,
    pub usd: f64,
}

impl Totals {
    pub fn add(&mut self, r: &Totals) {
        (self.i, self.o, self.cr, self.cw, self.n, self.usd) =
            (self.i + r.i, self.o + r.o, self.cr + r.cr, self.cw + r.cw, self.n + r.n, self.usd + r.usd);
    }
    /// Every token the model processed.
    pub fn all(&self) -> u64 {
        self.i + self.o + self.cr + self.cw
    }
}

/// One transcript's own totals: overall and per local day.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FileTotals {
    pub total: Totals,
    pub days: BTreeMap<String, Totals>,
    pub cwd: Option<String>,
    pub slug: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Biggest {
    pub o: u64,
    pub ts: u64,
    pub model: String,
}

/// Everything counted so far, across all transcripts.
#[derive(Default)]
pub struct Usage {
    seen: HashSet<String>,
    tool_ids: HashSet<String>,
    /// (local day, priced model id, from a subagent) → totals.
    pub day: HashMap<(String, String, bool), Totals>,
    /// (local "YYYY-MM-DDTHH", family) → totals.
    pub hour: HashMap<(String, &'static str), Totals>,
    /// Minute (ms since the epoch / 60 000) → totals, the last two hours only.
    pub minute: BTreeMap<u64, Totals>,
    pub tools: HashMap<String, u64>,
    pub unpriced: HashMap<String, u64>,
    pub biggest: Option<Biggest>,
    /// The earliest reply, in ms since the epoch.
    pub first: u64,
    /// Grows with every counted reply: a cheap "did anything change".
    pub version: u64,
}

pub fn day_key(ms: u64) -> String {
    Local.timestamp_millis_opt(ms as i64).single().map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string())
}

impl Usage {
    /// Counts one assistant line `j` of a transcript whose totals are `file`. `agent`: the transcript is a subagent's.
    pub fn add(&mut self, file: &mut FileTotals, j: &Value, agent: bool, now_ms: u64) {
        let Some(m) = j.get("message") else { return };
        for b in m.get("content").and_then(Value::as_array).into_iter().flatten() {
            if b.get("type").and_then(Value::as_str) == Some("tool_use")
                && let (Some(id), Some(name)) = (b.get("id").and_then(Value::as_str), b.get("name").and_then(Value::as_str))
                && self.tool_ids.insert(id.to_string())
            {
                *self.tools.entry(name.rsplit("__").next().unwrap_or(name).to_string()).or_default() += 1;
            }
        }
        let (Some(id), Some(model), Some(u)) =
            (m.get("id").and_then(Value::as_str), m.get("model").and_then(Value::as_str), m.get("usage"))
        else {
            return;
        };
        if model == "<synthetic>" || !self.seen.insert(id.to_string()) {
            return;
        }
        let ts = j.get("timestamp").and_then(Value::as_str).and_then(crate::sessions::iso_ms).unwrap_or(0);
        let usd = cost(model, u);
        if usd.is_none() {
            *self.unpriced.entry(model.to_string()).or_default() += 1;
        }
        let r = Totals {
            i: n(u, "input_tokens") as u64,
            o: n(u, "output_tokens") as u64,
            cr: n(u, "cache_read_input_tokens") as u64,
            cw: n(u, "cache_creation_input_tokens") as u64,
            n: 1,
            usd: usd.unwrap_or(0.0),
        };
        let local = Local.timestamp_millis_opt(ts as i64).single();
        let day = local.map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string());
        let hour = local.map_or_else(String::new, |d| format!("{day}T{:02}", d.hour()));
        self.day.entry((day.clone(), model_id(model).to_string(), agent)).or_default().add(&r);
        self.hour.entry((hour, family(model))).or_default().add(&r);
        if now_ms.saturating_sub(ts) < 2 * 3_600_000 {
            self.minute.entry(ts / 60_000).or_default().add(&r);
        }
        file.total.add(&r);
        file.days.entry(day).or_default().add(&r);
        if file.cwd.is_none() {
            file.cwd = j.get("cwd").and_then(Value::as_str).map(str::to_string);
            file.slug = j.get("slug").and_then(Value::as_str).map(str::to_string);
        }
        if self.biggest.as_ref().is_none_or(|b| r.o > b.o) {
            self.biggest = Some(Biggest { o: r.o, ts, model: model.to_string() });
        }
        if self.first == 0 || ts < self.first {
            self.first = ts;
        }
        self.version += 1;
    }

    /// Drops minutes older than two hours.
    pub fn trim_minutes(&mut self, now_ms: u64) {
        let keep = now_ms / 60_000 - 120;
        self.minute.retain(|m, _| *m >= keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prices() {
        let close = |a: Option<f64>, b: f64| assert!((a.unwrap() - b).abs() < 1e-9, "{a:?} vs {b}");
        close(cost("claude-opus-5-5", &json!({ "output_tokens": 1e6 })), 20.0);
        close(cost("claude-opus-5-5", &json!({ "cache_read_input_tokens": 1e6 })), 0.2);
        close(cost("claude-opus-5-5", &json!({ "output_tokens": 1e6, "speed": "fast" })), 40.0);
        close(
            cost(
                "claude-opus-4-8[1m]",
                &json!({ "cache_creation_input_tokens": 1e6, "cache_creation": { "ephemeral_1h_input_tokens": 1e6 } }),
            ),
            10.0,
        );
        close(cost("claude-haiku-5-5", &json!({ "input_tokens": 2e5 })), 0.1);
        assert_eq!(cost("gpt-9", &json!({ "output_tokens": 1 })), None);
        assert_eq!(model_id("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
    }

    #[test]
    fn one_reply_counts_once() {
        let mut u = Usage::default();
        let mut f = FileTotals::default();
        let line = json!({ "type": "assistant", "timestamp": "2026-10-30T12:00:00Z",
            "message": { "id": "msg_1", "model": "claude-opus-5", "usage": { "output_tokens": 1e6 }, "content": [{ "type": "tool_use", "id": "tu1", "name": "Bash" }] } });
        u.add(&mut f, &line, false, 0);
        u.add(&mut f, &line, false, 0);
        assert_eq!((f.total.o, f.total.usd, u.tools["Bash"], u.version), (1_000_000, 25.0, 1, 1));
    }
}
