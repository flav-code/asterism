//! `mdb top [--json]`: who uses the RAM and CPU right now, by agent session, Docker and app.
//! `mdb serve [--lang <code>]`: the agent the app starts over SSH on another machine (see `serve`).
use asterism_core::Monitor;
use asterism_core::attribute::{OwnerKind, Report};
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        serve(&args);
    }
    if args.first().map(String::as_str) != Some("top") {
        eprintln!("usage: mdb top [--json] | mdb serve [--lang <code>]");
        std::process::exit(2);
    }
    // Two samples a second apart: CPU figures are the average over that second.
    let mut monitor = Monitor::default();
    monitor.sample().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let report = monitor.sample().await;
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&report).expect("a report always serializes"));
    } else {
        print_report(&report);
    }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn print_report(r: &Report) {
    let m = &r.host.memory;
    let pressure = m.pressure.map_or(String::new(), |p| format!(", pressure {p:?}").to_lowercase());
    println!(
        "Machine: {:.0}% CPU of {} cores, {} of {} used{pressure}, swap {} of {}",
        r.host.cpu,
        r.host.ncpu,
        gb(m.used),
        gb(m.total),
        gb(m.swap_used),
        gb(m.swap_total)
    );
    if let Some(d) = &r.docker {
        let vm = d.vm.as_ref().map_or(String::new(), |v| format!(", VM {} on the host ({} beyond containers)", gb(v.mem), gb(v.other_mem)));
        println!("Docker:  {} containers, {} {}{vm}", d.containers, gb(d.containers_mem), d.name);
    }
    println!("\nWho uses it");
    for o in r.owners.iter().take(12) {
        let kind = match o.kind {
            OwnerKind::Session => "session",
            OwnerKind::Docker => "docker",
            OwnerKind::App => "app",
            OwnerKind::System => "system",
            OwnerKind::Other => "other",
        };
        println!("  {:>8}  {:>5.0}%  {:<8} {}", gb(o.mem), o.cpu, kind, o.name);
    }
    println!("\nSessions");
    for s in &r.sessions {
        println!(
            "  {:>8}  {:>5.0}%  {} ({}, {})",
            gb(s.mem + s.docker_mem),
            s.cpu + s.docker_cpu,
            s.title,
            s.project,
            s.status.as_deref().unwrap_or("?")
        );
        if let Some(a) = &s.activity {
            let cost = a.mem.map_or(String::new(), |m| format!(" — {}, {:.0}%", gb(m), a.cpu.unwrap_or(0.0)));
            println!("            now: {} {}{cost}", a.tool, a.detail);
        }
        for b in &s.builds {
            println!("            build: {} ({}, {:.0}%)", b.cmd, gb(b.mem), b.cpu);
        }
        for g in &s.groups {
            println!(
                "            {} {}: {} containers, {}, {:.0}% ({})",
                g.kind,
                g.name,
                g.containers.len(),
                gb(g.mem),
                g.cpu,
                g.why.unwrap_or("?")
            );
        }
    }
    for g in &r.unattached {
        let why = if g.shared > 0 { format!(", {} sessions work in its folders", g.shared) } else { String::new() };
        println!("  not attached: {} {} ({} containers, {}{why})", g.kind, g.name, g.containers.len(), gb(g.mem));
    }
}

/// The agent: this machine's session map and token usage for an app on another computer, read here so only the result
/// travels: one JSON frame per line on stdout (see `asterism_core::remote`). It stops when its input closes, which is
/// when the SSH connection is gone.
fn serve(args: &[String]) -> ! {
    use asterism_core::remote::{Frame, Sender};
    if let Some(lang) = args.iter().position(|a| a == "--lang").and_then(|i| args.get(i + 1)) {
        asterism_core::i18n::set(lang);
    }
    std::thread::spawn(|| {
        std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).ok();
        std::process::exit(0);
    });
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut write = move |frames: &[Frame]| {
        frames
            .iter()
            .try_for_each(|f| writeln!(out, "{}", serde_json::to_string(f).expect("frames always serialize")))
            .and_then(|_| out.flush())
            .is_ok()
    };
    if !write(&[Frame::Asterism(env!("CARGO_PKG_VERSION").into())]) {
        std::process::exit(0);
    }
    #[allow(deprecated)] // fixed on every OS since Rust 1.85
    let home = std::env::home_dir().unwrap_or_default();
    let (mut sender, mut last_usage, mut sent) = (Sender::default(), std::ptr::null(), Instant::now() - Duration::from_secs(9));
    asterism_core::graph::follow(&home, |graph, usage| {
        // While the history is first read, the map changes at every step: once a second is enough.
        if !usage.ready && sent.elapsed() < Duration::from_secs(1) {
            return true;
        }
        let mut frames: Vec<Frame> = sender.map(&graph).into_iter().collect();
        if Arc::as_ptr(&usage) != last_usage {
            last_usage = Arc::as_ptr(&usage);
            frames.extend(sender.usage(&usage));
        }
        if frames.is_empty() {
            return true;
        }
        sent = Instant::now();
        write(&frames)
    });
    std::process::exit(0);
}
