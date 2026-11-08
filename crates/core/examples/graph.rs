//! The map and usage on this machine, to compare with other tools: `cargo run --release -p asterism-core --example graph`.
use asterism_core::{desktop::DesktopStore, graph::Visualizer};
use std::collections::{BTreeMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn main() {
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap();
    let mut sys = sysinfo::System::new().unwrap();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let live: HashSet<u32> = sys.processes().keys().map(|p| p.as_u32()).collect();
    let (mut store, mut viz) = (DesktopStore::default(), Visualizer::default());
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    let start = Instant::now();
    let mut builds = 0;
    let graph = loop {
        let t = Instant::now();
        let desktop = store.refresh(&home);
        let g = viz.build(&home, &desktop, &live, now());
        builds += 1;
        let u = viz.usage_view();
        if builds <= 2 || u.ready {
            println!("build {builds}: {:?}, {} of {} transcripts", t.elapsed(), u.scanned, u.total);
        }
        if u.ready {
            break g;
        }
    };
    println!("full scan in {:?} over {builds} builds", start.elapsed());
    let _ = graph;
    let t = Instant::now();
    let desktop = store.refresh(&home);
    let graph = viz.build(&home, &desktop, &live, now()); // every session's transcript, now that all were read once
    println!("a build once ready: {:?}", t.elapsed());
    let mut ids = BTreeMap::new();
    for m in &graph.messages {
        *ids.entry(m.id.as_str()).or_insert(0) += 1;
    }
    let dups: Vec<_> = ids.iter().filter(|(_, c)| **c > 1).collect();
    println!("messages {}, ids seen twice or more: {}", graph.messages.len(), dups.len());
    for (id, c) in dups.iter().take(5) {
        let ms: Vec<_> = graph
            .messages
            .iter()
            .filter(|m| m.id == **id)
            .map(|m| (m.from.as_str(), m.to.as_str(), m.ts, format!("{:?}", m.kind)))
            .collect();
        println!("  {id} ×{c}: {ms:?}");
    }
    let mut kinds = BTreeMap::new();
    for n in &graph.nodes {
        *kinds.entry(format!("{:?}", n.kind)).or_insert(0) += 1;
    }
    let u = viz.usage_view();
    let usd: f64 = u.days.iter().map(|d| d.t.usd).sum();
    let replies: u64 = u.days.iter().map(|d| d.t.n).sum();
    println!("nodes {kinds:?}, edges {}, messages {}", graph.edges.len(), graph.messages.len());
    println!("usage: ${usd:.0} over {replies} replies, {} sessions", u.sessions.len());
}
