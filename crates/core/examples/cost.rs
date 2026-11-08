//! Where a sample's time goes: `cargo run --release -p mdb-core --example cost`.
use mdb_core::{desktop::DesktopStore, docker::DockerSampler, host::HostSampler, sessions};
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap();
    let (mut host, mut docker, mut reader, mut store) =
        (HostSampler::default(), DockerSampler::default(), sessions::SessionReader::default(), DesktopStore::default());
    for round in 0..3 {
        let t = Instant::now();
        let procs = host.processes();
        let a = t.elapsed();
        host.host();
        let b = t.elapsed();
        let d = docker.sample(&home).await.map_or(0, |d| d.containers.len());
        let c = t.elapsed();
        let desktop = store.refresh(&home);
        let s = reader.read(&home, &desktop, 1_794_100_000_000);
        let e = t.elapsed();
        println!(
            "round {round}: {} procs {:?} · host {:?} · docker ({d}) {:?} · sessions ({}) {:?}",
            procs.len(),
            a,
            b - a,
            c - b,
            s.len(),
            e - c
        );
    }
}
