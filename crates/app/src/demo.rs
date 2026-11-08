//! Demo mode (snapshot builds, `MDB_DEMO=1`): the real map, with every name made up. Session titles, projects,
//! branches, folders, messages and commands are replaced by invented ones (the same session always gets the same),
//! so screenshots show the shape of real work without saying what it is.
use asterism_core::graph::{Graph, NodeKind};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

const TITLES: &[&str] = &[
    "Fix the login redirect loop",
    "Add dark mode to settings",
    "Speed up the search index",
    "Refactor payment webhooks",
    "Write the onboarding guide",
    "Move jobs to the new queue",
    "Profile memory at startup",
    "Fix flaky end-to-end tests",
    "Add CSV export",
    "Upgrade the build tooling",
    "Design the notifications page",
    "Cache API responses",
    "Clean up feature flags",
    "Find why the dashboard is slow",
    "Write the release notes",
    "Add rate limiting",
    "Fix image upload on mobile",
    "Translate the help pages",
    "Split the auth service",
    "Check dependency licenses",
    "Add keyboard shortcuts",
    "Tune database indexes",
    "Fix the timezone bug in reports",
    "Prototype offline mode",
    "Review the API changes",
    "Add billing alerts",
    "Rewrite the importer",
    "Draw the architecture diagram",
    "Fix the websocket reconnect",
    "Add audit logs",
    "Migrate to the new router",
    "Polish the empty states",
    "Add a health endpoint",
    "Make the CLI faster",
    "Set up error tracking",
    "Fix the CSV encoding",
];
const AGENTS: &[&str] = &["Explore the codebase", "Run the test suite", "Review the diff", "Search the docs", "Check the logs"];
const PROJECTS: &[&str] = &["web", "api", "mobile", "infra", "docs", "design", "data", "cli", "site", "lab", "sdk", "ops"];
const SAYS: &[&str] =
    &["Status update", "Review done, two comments", "Tests pass", "Need a decision on the schema", "Merged", "Found the cause"];

fn pick<'a>(list: &'a [&'a str], key: &str) -> &'a str {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    list[(h.finish() % list.len() as u64) as usize]
}

pub fn on() -> bool {
    std::env::var("MDB_DEMO").is_ok()
}

/// The same graph with every name made up.
pub fn graph(g: &Graph) -> Graph {
    let mut g = g.clone();
    // Projects by size, so the biggest is always "web".
    let mut sizes: HashMap<String, usize> = HashMap::new();
    for n in &g.nodes {
        *sizes.entry(n.project.clone()).or_default() += 1;
    }
    let mut order: Vec<(String, usize)> = sizes.into_iter().collect();
    order.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let project: HashMap<String, String> = order
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.clone(), PROJECTS.get(i).map_or(format!("project-{i}"), |s| s.to_string())))
        .collect();
    for n in &mut g.nodes {
        n.title = match n.kind {
            NodeKind::Agent => pick(AGENTS, &n.id).to_string(),
            NodeKind::Remote => "Session on another machine".to_string(),
            NodeKind::Ghost => "Closed session".to_string(),
            _ => pick(TITLES, &n.id).to_string(),
        };
        n.project = project.get(&n.project).cloned().unwrap_or_else(|| "web".into());
        (n.cwd, n.branch, n.profile, n.pid) = (None, None, None, None);
        n.prs.clear();
        if let Some(m) = n.now.as_mut() {
            m.detail = match m.tool.as_str() {
                "Bash" => "cargo test".into(),
                "Read" | "Edit" | "Write" => "src/lib.rs".into(),
                _ => "…".into(),
            };
            m.tool = if m.tool.starts_with("mcp__") { "Tool".into() } else { m.tool.clone() };
        }
    }
    for m in &mut g.messages {
        let said = pick(SAYS, &m.id).to_string();
        (m.summary, m.text, m.note) = (said.clone(), said, String::new());
    }
    g
}
