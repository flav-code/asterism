//! Attribution on recorded data: a process table, `docker inspect` output and session files, as plain JSON.
use asterism_core::attribute::{Claim, Client, Group, OwnerKind, attach, attribute, group_containers};
use asterism_core::docker::{Container, DockerInfo, summarize};
use asterism_core::host::{Host, Proc};
use asterism_core::sessions::{Activity, SessionMeta};
use asterism_core::{Inputs, Report};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Deserialize)]
struct RecordedContainer {
    inspect: Value,
    cpu: f32,
    mem: u64,
}

#[derive(Deserialize)]
struct Fixture {
    home: String,
    at: u64,
    host: Host,
    docker: DockerInfo,
    procs: Vec<Proc>,
    sessions: Vec<SessionMeta>,
    activities: HashMap<String, Activity>,
    containers: Vec<RecordedContainer>,
}

fn evening() -> (Report, String) {
    let f: Fixture = serde_json::from_str(include_str!("fixtures/evening.json")).unwrap();
    let containers = f.containers.into_iter().map(|c| Container { cpu: c.cpu, mem: c.mem, ..summarize(&c.inspect) }).collect();
    // what the Bash call that runs the tests said it was for
    let said = HashMap::from([(
        "aaaaaaaa-0000-4000-8000-000000000001".to_string(),
        HashMap::from([(
            "docker run --rm -v /Users/me/dev/app-convert:/app runtime:local npx vitest run".to_string(),
            "Run the tests in /Users/me/dev/app-convert".to_string(),
        )]),
    )]);
    let report = attribute(Inputs {
        at: f.at,
        host: f.host,
        procs: &f.procs,
        sessions: &f.sessions,
        activities: &f.activities,
        said: &said,
        containers,
        docker: Some(f.docker),
        home: &f.home,
    });
    let json = serde_json::to_string(&report).unwrap();
    (report, json)
}

const GB: u64 = 1_000_000_000;

#[test]
fn every_session_gets_what_it_runs() {
    let (r, _) = evening();
    let titles: Vec<&str> = r.sessions.iter().map(|s| s.title.as_str()).collect();
    // biggest first, the ended session left out, the one missing from the registry found by its folder and start time
    assert_eq!(titles, ["Web images", "Convert replies to modules", "Tracker", "Dashboard", "Scratch notes"]);
    let s = |t: &str| r.sessions.iter().find(|s| s.title == t).unwrap();

    // tests in a container its docker command started, which is also what it is doing right now
    let convert = s("Convert replies to modules");
    assert_eq!(convert.groups.iter().map(|g| (g.name.as_str(), g.why)).collect::<Vec<_>>(), [("nice_neumann", Some("client"))]);
    assert!(convert.groups[0].containers[0].unlimited);
    assert_eq!(convert.builds.iter().map(|b| b.container.as_deref()).collect::<Vec<_>>(), [Some("nice_neumann")]);
    // its Bash call carries what the agent said it was for (home folder hidden), the docker command under it starts
    // the container, and the call's row counts what it started
    let call = convert.procs.iter().find(|p| p.pid == 301).unwrap();
    assert_eq!(call.what.as_deref(), Some("Run the tests in ~/dev/app-convert"));
    assert_eq!((convert.groups[0].client, call.tree_mem), (Some(302), 44_000_000));
    assert!(convert.procs.iter().all(|p| p.pid == 301 || p.what.is_none()));
    let now = convert.activity.as_ref().unwrap();
    assert_eq!((now.tool.as_str(), now.pid), ("Bash", Some(301)));
    assert_eq!(now.mem, Some(44_000_000 + 3_100_000_000));

    // a compile in its tree, the compiler under it counted once; a cluster mounting its worktree
    let web = s("Web images");
    assert_eq!(web.builds.len(), 1);
    assert_eq!((web.builds[0].pid, web.builds[0].mem), (Some(401), 1_700_000_000));
    assert!(web.builds[0].cmd.starts_with("cargo build --release --api-key=***"), "{}", web.builds[0].cmd);
    assert_eq!(
        web.groups.iter().map(|g| (g.kind.as_str(), g.name.as_str(), g.containers.len())).collect::<Vec<_>>(),
        [("Kubernetes", "web", 2)]
    );
    assert_eq!(web.mem, 2_350_000_000);

    // a dev server detached from the session (its parent is launchd), found by its folder; a Compose project by its folder
    let tracker = s("Tracker");
    assert!(tracker.procs.iter().any(|p| p.pid == 800 && p.detached));
    assert_eq!(tracker.mem, 3_050_000_000);
    assert_eq!(tracker.groups[0].name, "tracker");
    // idle: a tool call without a result was interrupted, not running
    assert!(tracker.activity.is_none());

    // three sessions work in the folder a cluster mounts: nobody gets it
    assert_eq!(r.unattached.iter().map(|g| (g.name.as_str(), g.shared)).collect::<Vec<_>>(), [("dev", 3)]);
}

#[test]
fn owners_add_up_without_counting_twice() {
    let (r, _) = evening();
    let owner = |kind: OwnerKind, name: &str| r.owners.iter().find(|o| o.kind == kind && o.name == name).map(|o| o.mem);
    // the VM holds every container: Docker keeps what the sessions don't already count
    let attached = 3_100_000_000 + 2_830_000_000 + 150_000_000;
    assert_eq!(owner(OwnerKind::Docker, "Example Linux VM"), Some(13 * GB - attached));
    assert_eq!(owner(OwnerKind::Session, "Web images"), Some(2_350_000_000 + 2_830_000_000));
    // an app counts its helpers (in nested bundles) and what it started outside any session
    assert_eq!(owner(OwnerKind::App, "Google Chrome"), Some(2_100_000_000));
    assert_eq!(owner(OwnerKind::App, "Claude"), Some(2_002_000_000));
    assert_eq!(owner(OwnerKind::App, "Terminal"), Some(153_000_000));
    assert_eq!(owner(OwnerKind::System, "System"), Some(1_050_000_000));
    assert_eq!(owner(OwnerKind::Other, "postgres"), Some(90_000_000));
    // an app is named by its outer bundle, which also gives its icon
    let bundle = |name: &str| r.owners.iter().find(|o| o.name == name).and_then(|o| o.bundle.clone());
    assert_eq!(bundle("Claude").as_deref(), Some("/Applications/Claude.app"));
    assert_eq!(bundle("Google Chrome").as_deref(), Some("/Applications/Google Chrome.app"));
    assert_eq!(bundle("postgres"), None);
    assert_eq!(r.owners[0].name, "Example Linux VM");
    let vm = r.docker.as_ref().unwrap().vm.as_ref().unwrap();
    assert_eq!(vm.other_mem, 13 * GB - 12_580_000_000);
}

#[test]
fn nothing_secret_comes_out() {
    let (_, json) = evening();
    for leak in ["hunter2", "SECRET-TOKEN", "/Users/me/"] {
        assert!(!json.contains(leak), "{leak} in the report");
    }
}

fn container(id: &str, name: &str, binds: &[&str], labels: &[(&str, &str)]) -> Container {
    Container {
        id: format!("{id:0<64}"),
        name: name.into(),
        binds: binds.iter().map(|b| b.to_string()).collect(),
        labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        ..Default::default()
    }
}

/// Each docker command with the second it started at; its process gets its place in the list, from 1.
fn claim(dirs: &[&str], visited: &[&str], clients: &[(&str, u64)]) -> Claim {
    let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
    let clients = clients.iter().zip(1..).map(|((cmd, start), pid)| Client { pid, start: *start, cmd: cmd.to_string() }).collect();
    Claim { dirs: own(dirs), visited: own(visited), clients }
}

#[test]
fn containers_go_to_the_session_that_runs_or_holds_them() {
    let compose = [("com.docker.compose.project", "tracker"), ("com.docker.compose.project.working_dir", "/home/me/tracker")];
    let mut groups: Vec<Group> = group_containers(vec![
        container("a1", "tests", &["/src/app-convert"], &[]), // A: its worktree (not /src/app's: a sibling with a longer name)
        container("b1", "build", &["/src/app/.worktrees/wt1", "/src/app/node_modules"], &[]), // B: the most specific folder
        container("c1", "k3d-web-server-0", &["/src/app-web"], &[("k3d.cluster", "web")]), // E: the whole cluster
        container("c2", "k3d-web-serverlb", &[], &[("k3d.cluster", "web")]),
        container("d1", "kind-control-plane", &["/opt/kind"], &[("io.x-k8s.kind.cluster", "kind")]), // no session folder holds it
        container("e1", "tracker-db", &[], &compose),                                                // C: the Compose folder
        container("e2", "tracker-app", &["/home/me/tracker"], &compose),
        container("f1", "shared", &["/src/app"], &[]), // B and D both work in /src/app: a tie, so unattached
        container("a2", "web-1", &[], &[]),            // D: its `docker exec` names it, though it mounts nothing
        container("ab", "redis", &[], &[]),            // nothing points at it
        container("ac", "old", &["/home/me/tracker-old"], &[]), // not C's: /home/me/tracker is only a prefix of the name
        container("ad", "conv", &["/src/app-conv2"], &[]), // A writes there, E's shell only went through: A
        container("ae", "scratch", &["/src/scratch/x"], &[]), // E only went through /src/scratch, but that beats everyone's /src
    ]);
    let claims = [
        claim(&["/src", "/src/app-convert", "/src/app-conv2"], &[], &[]),
        claim(&["/src", "/src/app/.worktrees/wt1", "/src/app"], &[], &[]),
        claim(&["/home/me/tracker"], &[], &[]),
        claim(&["/src", "/src/app"], &[], &[("docker ps", 0), ("docker exec -it web-1 sh", 0)]),
        claim(&["/src/app-web"], &["/src/app-conv2", "/src/scratch"], &[]),
    ];
    attach(&mut groups, &claims);
    let who = |g: &Group| match (g.owner, g.shared) {
        (Some(i), _) => ["A", "B", "C", "D", "E"][i].to_string(),
        (None, n) if n > 0 => format!("tie {n}"),
        _ => "-".into(),
    };
    let got: Vec<String> = groups.iter().map(|g| format!("{} {} {}: {}", g.kind, g.name, g.containers.len(), who(g))).collect();
    assert_eq!(
        got,
        [
            "Container tests 1: A",
            "Container build 1: B",
            "Kubernetes web 2: E",
            "Kubernetes kind 1: -",
            "Compose tracker 2: C",
            "Container shared 1: tie 2",
            "Container web-1 1: D",
            "Container redis 1: -",
            "Container old 1: -",
            "Container conv 1: A",
            "Container scratch 1: E",
        ]
    );
    assert_eq!(groups[1].dir.as_deref(), Some("/src/app/.worktrees/wt1"));
    assert_eq!(groups[6].client, Some(2));
}

#[test]
fn a_container_goes_to_the_command_that_started_it() {
    // two `docker run` of one session mount the same folder: each container is the one of the command started just before it
    let started = |name: &str, at: u64| Container { started: at, ..container(name, name, &["/src/app"], &[]) };
    let mut groups = group_containers(vec![started("a1", 1_010), started("a2", 1_302)]);
    let run = "docker run --rm -v /src/app:/work img";
    attach(&mut groups, &[claim(&[], &[], &[(run, 1_000), (run, 1_300)])]);
    assert_eq!(groups.iter().map(|g| g.client).collect::<Vec<_>>(), [Some(1), Some(2)]);
}
