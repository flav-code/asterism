//! What the views share: the filters, and which sessions, links and messages they let through.
use gpui_kit::*;
use mdb_core::graph::{Graph, Node, NodeKind};
use std::collections::{HashMap, HashSet};

pub const PALETTE: [u32; 8] = [0x7c9cff, 0xff9f6b, 0xe879f9, 0xa3e635, 0x2dd4bf, 0xfacc15, 0xfda4af, 0x93c5fd];
pub const RANGES: [(&str, Option<f64>); 7] = [
    ("6h", Some(6.0)),
    ("24h", Some(24.0)),
    ("3d", Some(72.0)),
    ("7d", Some(168.0)),
    ("14d", Some(336.0)),
    ("30d", Some(720.0)),
    ("All", None),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Filters {
    /// Hours back; None is everything.
    pub range_h: Option<f64>,
    pub project: Option<String>,
    /// Lowercase, without accents.
    pub query: String,
    pub archived: bool,
    pub agents: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Filters { range_h: Some(24.0), project: None, query: String::new(), archived: true, agents: true }
    }
}

pub fn is_session(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Desktop | NodeKind::Cli)
}

pub fn working(n: &Node) -> bool {
    n.live && (n.status == "busy" || n.status == "shell")
}

/// Lowercase without accents, so "resume" finds "Résumé".
pub fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' => 'i',
            'ô' | 'ö' | 'ó' | 'õ' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

pub struct Visible {
    /// Indexes into the graph's nodes.
    pub nodes: Vec<usize>,
    pub edges: Vec<usize>,
    pub messages: Vec<usize>,
    /// Where the range starts, in ms (for "All", the oldest visible session).
    pub start: u64,
}

pub fn visible(g: &Graph, f: &Filters, now_ms: u64) -> Visible {
    let start = f.range_h.map_or(0, |h| now_ms.saturating_sub((h * 3_600_000.0) as u64));
    let by_id: HashMap<&str, usize> = g.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let mut vis: HashSet<usize> = HashSet::new();
    for (i, n) in g.nodes.iter().enumerate() {
        let text = || norm(&format!("{} {} {}", n.title, n.project, n.branch.as_deref().unwrap_or_default()));
        if !is_session(n)
            || f.project.as_ref().is_some_and(|p| &n.project != p)
            || (!f.query.is_empty() && !text().contains(&f.query))
            || !(n.live || n.last_at >= start)
            || (n.archived && !f.archived && !n.live)
        {
            continue;
        }
        vis.insert(i);
    }
    if f.agents {
        for (i, n) in g.nodes.iter().enumerate() {
            let parent_shown = n.parent.as_deref().and_then(|p| by_id.get(p)).is_some_and(|p| vis.contains(p));
            if n.kind == NodeKind::Agent && parent_shown && (n.live || n.last_at >= start) {
                vis.insert(i);
            }
        }
    }
    let in_range: Vec<usize> = (0..g.messages.len()).filter(|&i| g.messages[i].ts >= start).collect();
    for &m in &in_range {
        let msg = &g.messages[m];
        let (from, to) = (by_id.get(msg.from.as_str()).copied(), by_id.get(msg.to.as_str()).copied());
        let touches = from.is_some_and(|i| vis.contains(&i)) || to.is_some_and(|i| vis.contains(&i));
        for i in [from, to].into_iter().flatten() {
            let n = &g.nodes[i];
            if touches && matches!(n.kind, NodeKind::Ghost | NodeKind::Remote) || (touches && n.kind == NodeKind::Agent && f.agents) {
                vis.insert(i);
            }
        }
    }
    let shown = |id: &str| by_id.get(id).is_some_and(|i| vis.contains(i));
    let messages = in_range.into_iter().filter(|&m| shown(&g.messages[m].from) && shown(&g.messages[m].to)).collect();
    let edges = (0..g.edges.len()).filter(|&e| shown(&g.edges[e].from) && shown(&g.edges[e].to)).collect();
    let mut nodes: Vec<usize> = vis.into_iter().collect();
    nodes.sort_unstable();
    let start = if f.range_h.is_some() {
        start
    } else {
        nodes
            .iter()
            .map(|&i| g.nodes[i].created_at.max(1).min(g.nodes[i].last_at.max(1)))
            .min()
            .unwrap_or(now_ms.saturating_sub(86_400_000))
    };
    Visible { nodes, edges, messages, start }
}

/// One color per project, the busiest first, in a fixed order.
pub fn project_colors(g: &Graph) -> HashMap<String, Rgba> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in g.nodes.iter().filter(|n| is_session(n)) {
        *counts.entry(&n.project).or_default() += 1;
    }
    let mut list: Vec<(&str, usize)> = counts.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    list.into_iter().enumerate().map(|(i, (p, _))| (p.to_string(), rgb(PALETTE[i % PALETTE.len()]))).collect()
}
