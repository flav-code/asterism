//! What each AI coding agent session runs on this machine, and what it costs in RAM and CPU.
//! Read-only: it lists processes, reads Claude Code's session files and asks the Docker engine; it changes nothing.
pub mod attribute;
pub mod desktop;
pub mod docker;
pub mod graph;
pub mod host;
pub mod i18n;
pub mod sessions;
pub mod text;
pub mod transcripts;
pub mod usage;

pub use attribute::{Inputs, Report, attribute};

rust_i18n::i18n!("../../locales", fallback = "en");

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Samples the machine. Each sample's CPU figures cover the time since the previous one, so keep one `Monitor`
/// and call `sample` at a steady pace; the first sample's CPU figures are empty.
pub struct Monitor {
    host: host::HostSampler,
    docker: docker::DockerSampler,
    sessions: sessions::SessionReader,
    desktop: desktop::DesktopStore,
    home: PathBuf,
    /// Transcript → (its size when read, what it showed): a transcript is read again only when it grows.
    activities: HashMap<PathBuf, (u64, Option<sessions::Activity>)>,
}

impl Default for Monitor {
    fn default() -> Self {
        #[allow(deprecated)] // fixed on every OS since Rust 1.85; the deprecation note lags behind
        let home = std::env::home_dir().unwrap_or_default();
        Self {
            host: Default::default(),
            docker: Default::default(),
            sessions: Default::default(),
            desktop: Default::default(),
            home,
            activities: HashMap::new(),
        }
    }
}

impl Monitor {
    pub async fn sample(&mut self) -> Report {
        let procs = self.host.processes();
        let host = self.host.host();
        let docker = self.docker.sample(&self.home).await;
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let desktop = self.desktop.refresh(&self.home);
        let sessions = self.sessions.read(&self.home, &desktop, at);

        let mut activities = HashMap::new();
        let mut cache = HashMap::new();
        for s in sessions.iter().filter(|s| s.pid.is_some()) {
            let Some(t) = &s.transcript else { continue };
            let len = std::fs::metadata(t).map_or(0, |m| m.len());
            let a = match self.activities.remove(t) {
                Some((l, a)) if l == len => a,
                _ => sessions::current_activity(t),
            };
            if let Some(a) = &a {
                activities.insert(s.id.clone(), a.clone());
            }
            cache.insert(t.clone(), (len, a));
        }
        self.activities = cache;

        let (info, containers) = docker.map(|d| (d.info, d.containers)).unwrap_or_default();
        let home = self.home.to_string_lossy();
        attribute(Inputs { at, host, procs: &procs, sessions: &sessions, activities: &activities, containers, docker: info, home: &home })
    }
}
