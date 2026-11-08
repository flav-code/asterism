//! Demo mode (snapshot builds, `MDB_DEMO=1`): the real map, with every name made up. Session titles, projects,
//! branches, folders, messages and commands are replaced by invented ones (the same session always gets the same),
//! so screenshots show the shape of real work without saying what it is.
use asterism_core::graph::{Graph, NodeKind};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Titles are a verb and a thing: 12 × 30 = 360 different ones, so sessions rarely share one.
const VERBS: &[&str] =
    &["Fix", "Add", "Speed up", "Refactor", "Document", "Test", "Review", "Redesign", "Migrate", "Profile", "Clean up", "Ship"];
const THINGS: &[&str] = &[
    "the login flow",
    "search",
    "payments",
    "the onboarding",
    "the job queue",
    "memory use",
    "end-to-end tests",
    "CSV export",
    "the build",
    "notifications",
    "the API cache",
    "feature flags",
    "the dashboard",
    "release notes",
    "rate limits",
    "image upload",
    "the help pages",
    "the auth service",
    "dependencies",
    "keyboard shortcuts",
    "database indexes",
    "reports",
    "offline mode",
    "billing",
    "the importer",
    "audit logs",
    "the router",
    "empty states",
    "the CLI",
    "error tracking",
];
/// Tools named as they are everywhere; any other (from an MCP server, say) is just "Tool".
const TOOLS: &[&str] = &["Bash", "Read", "Edit", "Write", "Grep", "Glob", "Task", "WebFetch", "WebSearch", "reply"];
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
    // Sessions in a fixed order, each with its own title: stepping by a number prime to 360 visits every one.
    let mut ids: Vec<&str> =
        g.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Desktop | NodeKind::Cli)).map(|n| n.id.as_str()).collect();
    ids.sort_unstable();
    let count = VERBS.len() * THINGS.len();
    let titles: HashMap<String, String> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let k = (i * 157 + 11) % count;
            (id.to_string(), format!("{} {}", VERBS[k % VERBS.len()], THINGS[k / VERBS.len()]))
        })
        .collect();
    for n in &mut g.nodes {
        n.title = match n.kind {
            NodeKind::Agent => pick(AGENTS, &n.id).to_string(),
            NodeKind::Remote => "Session on another machine".to_string(),
            NodeKind::Ghost => "Closed session".to_string(),
            _ => titles.get(&n.id).cloned().unwrap_or_else(|| "Fix the build".into()),
        };
        n.project = project.get(&n.project).cloned().unwrap_or_else(|| "web".into());
        (n.cwd, n.branch, n.profile, n.pid) = (None, None, None, None);
        n.prs.clear();
        if let Some(m) = n.now.as_mut() {
            m.detail = match m.tool.as_str() {
                "reply" => pick(SAYS, &n.id).to_string(),
                "Bash" => "cargo test".into(),
                "Read" | "Edit" | "Write" => "src/lib.rs".into(),
                _ => "…".into(),
            };
            m.tool = if TOOLS.contains(&m.tool.as_str()) { m.tool.clone() } else { "Tool".into() };
        }
    }
    for m in &mut g.messages {
        let said = pick(SAYS, &m.id).to_string();
        (m.summary, m.text, m.note) = (said.clone(), said, String::new());
    }
    g
}
