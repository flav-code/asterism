//! Docker, through the engine's API (never the CLI): running containers, their limits, mounts and usage.
//! Optional: when no engine answers, the probe says so and tries again a minute later.
use bollard::Docker;
use bollard::query_parameters::StatsOptionsBuilder;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

pub const COMPOSE_PROJECT: &str = "com.docker.compose.project";
pub const COMPOSE_DIR: &str = "com.docker.compose.project.working_dir";
pub const COMPOSE_FILES: &str = "com.docker.compose.project.config_files";
/// Container labels that put containers in one row: local Kubernetes clusters, then Compose projects.
pub const GROUP_LABELS: &[(&str, &str)] =
    &[("k3d.cluster", "Kubernetes"), ("io.x-k8s.kind.cluster", "Kubernetes"), (COMPOSE_PROJECT, "Compose")];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub cmd: String,
    /// Start time, in seconds since the epoch.
    pub started: u64,
    /// CPU limit in cores, 0 when there is none.
    pub cpus: f64,
    /// Memory limit in bytes, 0 when there is none.
    pub mem_limit: u64,
    pub binds: Vec<String>,
    /// Only the labels used to group and attach: others can hold secrets (a cluster token).
    pub labels: BTreeMap<String, String>,
    /// Percent of one core.
    pub cpu: f32,
    pub mem: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DockerInfo {
    pub name: String,
    pub ncpu: u32,
    /// The memory the engine sees: the VM's size when Docker runs in one.
    pub mem: u64,
}

/// The parts of `docker inspect` that matter here.
pub fn summarize(c: &Value) -> Container {
    let hc = c.get("HostConfig").cloned().unwrap_or_default();
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let cpus = if num(&hc, "NanoCpus") > 0.0 {
        num(&hc, "NanoCpus") / 1e9
    } else if num(&hc, "CpuQuota") > 0.0 {
        num(&hc, "CpuQuota") / if num(&hc, "CpuPeriod") > 0.0 { num(&hc, "CpuPeriod") } else { 1e5 }
    } else {
        0.0
    };
    let config = c.get("Config").cloned().unwrap_or_default();
    let words = |k: &str| {
        config.get(k).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()
    };
    let all_labels = config.get("Labels").and_then(Value::as_object);
    let keep = GROUP_LABELS.iter().map(|(l, _)| *l).chain([COMPOSE_DIR, COMPOSE_FILES]);
    let labels = keep.filter_map(|k| Some((k.to_string(), all_labels?.get(k)?.as_str()?.to_string()))).collect();
    let binds = c
        .get("Mounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("Type").and_then(Value::as_str) == Some("bind"))
        .filter_map(|m| m.get("Source").and_then(Value::as_str).map(str::to_string))
        .collect();
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Container {
        id: s(c, "Id"),
        name: s(c, "Name").trim_start_matches('/').to_string(),
        image: s(&config, "Image"),
        cmd: [words("Entrypoint"), words("Cmd")].concat().join(" "),
        started: c.pointer("/State/StartedAt").and_then(Value::as_str).and_then(crate::sessions::iso_ms).map_or(0, |ms| ms / 1000),
        cpus,
        mem_limit: num(&hc, "Memory") as u64,
        binds,
        labels,
        cpu: 0.0,
        mem: 0,
    }
}

/// The engine the `docker` CLI would talk to: `DOCKER_HOST`, else the current context's, else the default.
pub fn engine_host(home: &Path) -> Option<String> {
    if let Ok(h) = std::env::var("DOCKER_HOST") {
        return Some(h);
    }
    let dir = home.join(".docker");
    let config: Value = serde_json::from_slice(&std::fs::read(dir.join("config.json")).ok()?).ok()?;
    let context = std::env::var("DOCKER_CONTEXT").ok().or_else(|| config.get("currentContext")?.as_str().map(str::to_string))?;
    if context == "default" {
        return None;
    }
    std::fs::read_dir(dir.join("contexts/meta")).ok()?.flatten().find_map(|e| {
        let meta: Value = serde_json::from_slice(&std::fs::read(e.path().join("meta.json")).ok()?).ok()?;
        (meta.get("Name")?.as_str()? == context).then(|| meta.pointer("/Endpoints/docker/Host")?.as_str().map(str::to_string))?
    })
}

#[derive(Default)]
pub struct DockerSampler {
    client: Option<Docker>,
    retry_at: Option<Instant>,
    inspected: HashMap<String, Container>,
    /// Container id → CPU time in ns at the previous sample.
    last_cpu: HashMap<String, (u64, Instant)>,
    info: Option<(Instant, DockerInfo)>,
}

pub struct DockerSample {
    pub info: Option<DockerInfo>,
    pub containers: Vec<Container>,
    pub error: Option<String>,
}

impl DockerSampler {
    /// None when no Docker engine answers.
    pub async fn sample(&mut self, home: &Path) -> Option<DockerSample> {
        if self.retry_at.is_some_and(|t| Instant::now() < t) {
            return None;
        }
        if self.client.is_none() {
            let client = match engine_host(home) {
                Some(h) => Docker::connect_with_host(&h),
                None => Docker::connect_with_defaults(),
            };
            self.client = client.ok();
        }
        let docker = self.client.clone()?;
        let list = match docker.list_containers(None).await {
            Ok(l) => l,
            Err(_) => {
                // No engine running: look again in a minute.
                self.client = None;
                self.retry_at = Some(Instant::now() + Duration::from_secs(60));
                return None;
            }
        };
        let ids: Vec<String> = list.into_iter().filter_map(|c| c.id).collect();
        self.inspected.retain(|id, _| ids.contains(id));
        self.last_cpu.retain(|id, _| ids.contains(id));
        let fresh: Vec<&String> = ids.iter().filter(|id| !self.inspected.contains_key(*id)).collect();
        for id in fresh {
            // A container that stopped in between is simply skipped.
            if let Some(v) = docker.inspect_container(id, None).await.ok().and_then(|c| serde_json::to_value(c).ok()) {
                self.inspected.insert(id.clone(), summarize(&v));
            }
        }
        if self.info.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(300))
            && let Ok(i) = docker.info().await
        {
            let info = DockerInfo {
                name: i.operating_system.unwrap_or_default(),
                ncpu: i.ncpu.and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
                mem: i.mem_total.and_then(|n| u64::try_from(n).ok()).unwrap_or(0),
            };
            self.info = Some((Instant::now(), info));
        }

        // One-shot counters, read now; CPU is their change since the previous sample.
        let options = StatsOptionsBuilder::new().stream(false).one_shot(true).build();
        let stats = futures_util::future::join_all(ids.iter().map(|id| {
            let docker = docker.clone();
            let options = options.clone();
            async move { docker.stats(id, Some(options)).next().await.and_then(Result::ok) }
        }))
        .await;
        let now = Instant::now();
        let mut containers = Vec::new();
        for (id, st) in ids.iter().zip(stats) {
            let (Some(mut c), Some(st)) = (self.inspected.get(id).cloned(), st) else { continue };
            let ns = st.cpu_stats.and_then(|s| s.cpu_usage).and_then(|u| u.total_usage).unwrap_or(0);
            if let Some((before, at)) = self.last_cpu.get(id) {
                let dt = now.duration_since(*at).as_nanos() as f64;
                c.cpu = if dt > 0.0 { (ns.saturating_sub(*before) as f64 / dt * 100.0) as f32 } else { 0.0 };
            }
            self.last_cpu.insert(id.clone(), (ns, now));
            if let Some(m) = st.memory_stats {
                // As `docker stats` counts it: usage minus the page cache that can be dropped.
                let cache =
                    m.stats.as_ref().and_then(|s| s.get("inactive_file").or_else(|| s.get("total_inactive_file")).copied()).unwrap_or(0);
                c.mem = m.usage.unwrap_or(0).saturating_sub(cache);
            }
            containers.push(c);
        }
        Some(DockerSample { info: self.info.as_ref().map(|(_, i)| i.clone()), containers, error: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inspect_summary_keeps_limits_mounts_and_safe_labels() {
        let c = summarize(&json!({
            "Id": "f".repeat(64), "Name": "/k3d-x-server-0",
            "Config": { "Image": "k3s", "Labels": { "k3d.cluster": "x", "k3d.cluster.token": "SECRET" }, "Cmd": ["server"] },
            "HostConfig": { "NanoCpus": 0, "Memory": 0 },
            "Mounts": [{ "Type": "bind", "Source": "/src/x" }, { "Type": "volume", "Source": "/var/lib/docker/volumes/v/_data" }],
            "State": { "StartedAt": "2026-11-07T01:00:00Z" }
        }));
        assert_eq!((c.name.as_str(), c.cpus, c.mem_limit, c.cmd.as_str()), ("k3d-x-server-0", 0.0, 0, "server"));
        assert_eq!(c.binds, ["/src/x"]);
        assert_eq!(c.labels.into_iter().collect::<Vec<_>>(), [("k3d.cluster".to_string(), "x".to_string())]);
        let limited = summarize(&json!({ "Id": "e", "HostConfig": { "NanoCpus": 4_000_000_000_u64, "Memory": 6_u64 << 30 } }));
        assert_eq!((limited.cpus, limited.mem_limit), (4.0, 6 << 30));
        assert_eq!(summarize(&json!({ "Id": "q", "HostConfig": { "CpuQuota": 150_000, "CpuPeriod": 100_000 } })).cpus, 1.5);
    }
}
