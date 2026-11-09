//! What the agent on another machine sends over SSH, one JSON frame per line: its map and token usage as patches.
//! Each item travels whole under its key; after the first frame (everything) only what changed does.
use crate::graph::{DayRow, Edge, Graph, HourRow, Msg, Node, SessionUsage, UsageView};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub trait Keyed {
    fn key(&self) -> String;
}

impl Keyed for Node {
    fn key(&self) -> String {
        self.id.clone()
    }
}
impl Keyed for Msg {
    fn key(&self) -> String {
        self.id.clone()
    }
}
impl Keyed for Edge {
    fn key(&self) -> String {
        format!("{}\t{}\t{:?}", self.from, self.to, self.kind)
    }
}
impl Keyed for DayRow {
    fn key(&self) -> String {
        format!("{}\t{}\t{}", self.d, self.model, self.agent)
    }
}
impl Keyed for HourRow {
    fn key(&self) -> String {
        format!("{}\t{}", self.h, self.fam)
    }
}
impl Keyed for SessionUsage {
    fn key(&self) -> String {
        self.id.clone()
    }
}

/// The items of a list that are new or changed, and the keys of those gone.
#[derive(Debug, Serialize, Deserialize)]
pub struct Patch<T> {
    #[serde(default = "Vec::new", skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<T>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub del: Vec<String>,
}

impl<T: Keyed> Patch<T> {
    fn is_empty(&self) -> bool {
        self.set.is_empty() && self.del.is_empty()
    }

    pub fn apply(self, list: &mut Vec<T>) {
        if !self.del.is_empty() {
            let del: HashSet<String> = self.del.into_iter().collect();
            list.retain(|x| !del.contains(&x.key()));
        }
        let mut at: HashMap<String, usize> = list.iter().enumerate().map(|(i, x)| (x.key(), i)).collect();
        for x in self.set {
            match at.get(&x.key()) {
                Some(&i) => list[i] = x,
                None => {
                    at.insert(x.key(), list.len());
                    list.push(x);
                }
            }
        }
    }
}

/// One line from the agent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    /// The first line: the agent's version.
    Asterism(String),
    Map {
        nodes: Patch<Node>,
        edges: Patch<Edge>,
        messages: Patch<Msg>,
    },
    /// `rest` is the usage without its days, hours and sessions, which come as patches.
    Usage {
        days: Patch<DayRow>,
        hours: Patch<HourRow>,
        sessions: Patch<SessionUsage>,
        rest: Box<UsageView>,
    },
}

/// What one list looked like when last sent: each item's JSON by key.
#[derive(Default)]
struct Sent(HashMap<String, String>);

impl Sent {
    fn diff<T: Keyed + Serialize + Clone>(&mut self, items: &[T]) -> Patch<T> {
        let mut now = HashMap::with_capacity(items.len());
        let mut set = Vec::new();
        for x in items {
            let (k, json) = (x.key(), serde_json::to_string(x).expect("map items always serialize"));
            if self.0.get(&k) != Some(&json) {
                set.push(x.clone());
            }
            now.insert(k, json);
        }
        let del = self.0.keys().filter(|k| !now.contains_key(*k)).cloned().collect();
        self.0 = now;
        Patch { set, del }
    }
}

/// The agent's side: turns each new map or usage into the frame of what changed since the last one (None: nothing).
#[derive(Default)]
pub struct Sender {
    nodes: Sent,
    edges: Sent,
    messages: Sent,
    days: Sent,
    hours: Sent,
    sessions: Sent,
    rest: String,
}

impl Sender {
    pub fn map(&mut self, g: &Graph) -> Option<Frame> {
        let (nodes, edges, messages) = (self.nodes.diff(&g.nodes), self.edges.diff(&g.edges), self.messages.diff(&g.messages));
        (!(nodes.is_empty() && edges.is_empty() && messages.is_empty())).then_some(Frame::Map { nodes, edges, messages })
    }

    pub fn usage(&mut self, u: &UsageView) -> Option<Frame> {
        let (days, hours, sessions) = (self.days.diff(&u.days), self.hours.diff(&u.hours), self.sessions.diff(&u.sessions));
        let rest = UsageView { days: Vec::new(), hours: Vec::new(), sessions: Vec::new(), ..u.clone() };
        let json = serde_json::to_string(&rest).expect("usage always serializes");
        if days.is_empty() && hours.is_empty() && sessions.is_empty() && json == self.rest {
            return None;
        }
        self.rest = json;
        Some(Frame::Usage { days, hours, sessions, rest: Box::new(rest) })
    }
}

/// The app's side: the other machine's map and usage, rebuilt from its frames.
#[derive(Default)]
pub struct Mirror {
    pub graph: Graph,
    pub usage: UsageView,
}

impl Mirror {
    pub fn apply(&mut self, frame: Frame) {
        match frame {
            Frame::Asterism(_) => {}
            Frame::Map { nodes, edges, messages } => {
                nodes.apply(&mut self.graph.nodes);
                edges.apply(&mut self.graph.edges);
                messages.apply(&mut self.graph.messages);
                self.graph.messages.sort_by_key(|m| m.ts);
            }
            Frame::Usage { days, hours, sessions, rest } => {
                let mut u = *rest;
                (u.days, u.hours, u.sessions) =
                    (std::mem::take(&mut self.usage.days), std::mem::take(&mut self.usage.hours), std::mem::take(&mut self.usage.sessions));
                days.apply(&mut u.days);
                hours.apply(&mut u.hours);
                sessions.apply(&mut u.sessions);
                self.usage = u;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, MsgKind};

    fn msg(id: &str, ts: u64, text: &str) -> Msg {
        Msg {
            id: id.into(),
            from: "a".into(),
            to: "b".into(),
            ts,
            summary: String::new(),
            text: text.into(),
            ok: None,
            note: String::new(),
            kind: MsgKind::Message,
        }
    }

    /// Frames through JSON, as over SSH, rebuild the agent's map; after the first, only changes travel.
    #[test]
    fn frames_rebuild_the_map() {
        let (mut sender, mut mirror) = (Sender::default(), Mirror::default());
        let mut send = |g: &Graph| {
            let frame = sender.map(g);
            let line = frame.as_ref().map(|f| serde_json::to_string(f).unwrap());
            if let Some(l) = &line {
                mirror.apply(serde_json::from_str(l).unwrap());
            }
            line
        };
        let edge = |to: &str| Edge { from: "a".into(), to: to.into(), kind: EdgeKind::Spawn, at: 1 };
        let mut g = Graph { messages: vec![msg("m1", 5, "hi"), msg("m2", 9, "yo")], edges: vec![edge("b"), edge("c")], ..Graph::default() };
        assert!(send(&g).is_some());
        assert!(send(&g).is_none(), "nothing changed, nothing sent");
        g.messages = vec![msg("m1", 5, "hi!"), msg("m3", 1, "early")];
        g.edges.remove(0);
        let line = send(&g).unwrap();
        assert!(!line.contains("m2") || line.contains("\"del\":[\"m2\"]"), "{line}");
        assert!(!line.contains("\"c\""), "an unchanged edge is not sent again: {line}");
        let ids: Vec<(&str, &str)> = mirror.graph.messages.iter().map(|m| (m.id.as_str(), m.text.as_str())).collect();
        assert_eq!(ids, [("m3", "early"), ("m1", "hi!")]);
        assert_eq!(mirror.graph.edges.iter().map(|e| e.to.as_str()).collect::<Vec<_>>(), ["c"]);
    }
}
