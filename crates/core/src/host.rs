//! The machine: its processes, CPU and memory. Each sample's CPU figures cover the time since the previous one.
use serde::{Deserialize, Serialize};
use std::time::Instant;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// The command line, arguments joined with spaces (as `ps` shows it).
    pub cmd: String,
    pub exe: String,
    pub cwd: Option<String>,
    /// Percent of one core.
    pub cpu: f32,
    /// Bytes: the physical footprint on macOS (Activity Monitor's "Memory"), else the resident size.
    pub mem: u64,
    /// Start time, in seconds since the epoch.
    pub start: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    Normal,
    Warning,
    Critical,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    pub total: u64,
    /// What Activity Monitor calls "Memory Used" on macOS: app memory, wired and compressed.
    pub used: u64,
    pub compressed: Option<u64>,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Bytes per second moved into and out of swap since the previous sample.
    pub swap_in: Option<f64>,
    pub swap_out: Option<f64>,
    pub pressure: Option<Pressure>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Host {
    pub ncpu: usize,
    /// Percent of one core, summed over all cores.
    pub cpu: f32,
    pub memory: Memory,
}

pub struct HostSampler {
    sys: System,
    /// Swap counters at the previous sample, to turn them into rates (macOS only so far).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    swap: Option<(Instant, u64, u64)>,
}

impl Default for HostSampler {
    fn default() -> Self {
        // Fails only on an OS sysinfo doesn't support, where nothing here could work.
        Self { sys: System::new().expect("sysinfo supports this OS"), swap: None }
    }
}

impl HostSampler {
    pub fn processes(&mut self) -> Vec<Proc> {
        // Never the environment: it holds secrets and is never shown.
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .without_tasks();
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        self.sys
            .processes()
            .values()
            .map(|p| {
                let pid = p.pid().as_u32();
                let cmd = p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" ");
                let name = p.name().to_string_lossy().into_owned();
                Proc {
                    pid,
                    ppid: p.parent().map_or(0, |q| q.as_u32()),
                    cmd: if cmd.is_empty() { name.clone() } else { cmd },
                    name,
                    exe: p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
                    cwd: p.cwd().map(|c| c.to_string_lossy().into_owned()).filter(|c| !c.is_empty()),
                    cpu: p.cpu_usage(),
                    mem: footprint(pid).unwrap_or_else(|| p.memory()),
                    start: p.start_time(),
                }
            })
            .collect()
    }

    pub fn host(&mut self) -> Host {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let ncpu = self.sys.cpus().len();
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))] // only macOS refines it below
        let mut memory = Memory {
            total: self.sys.total_memory(),
            used: self.sys.used_memory(),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
            ..Default::default()
        };
        #[cfg(target_os = "macos")]
        if let Some(v) = mac::vm_stats() {
            memory.used = v.used;
            memory.compressed = Some(v.compressed);
            let now = Instant::now();
            if let Some((at, ins, outs)) = self.swap {
                let dt = now.duration_since(at).as_secs_f64().max(1e-3);
                memory.swap_in = Some(v.swapins.saturating_sub(ins) as f64 * v.page as f64 / dt);
                memory.swap_out = Some(v.swapouts.saturating_sub(outs) as f64 * v.page as f64 / dt);
            }
            self.swap = Some((now, v.swapins, v.swapouts));
            memory.pressure = mac::pressure();
        }
        Host { ncpu, cpu: self.sys.global_cpu_usage() * ncpu as f32, memory }
    }
}

#[cfg(target_os = "macos")]
fn footprint(pid: u32) -> Option<u64> {
    use libproc::pid_rusage::{RUsageInfoV2, pidrusage};
    // Other users' processes refuse it: their resident size stands in.
    pidrusage::<RUsageInfoV2>(i32::try_from(pid).ok()?).ok().map(|r| r.ri_phys_footprint)
}

// shortcut: Windows shows the working set, not the private working set Task Manager shows; read it in M3.
#[cfg(not(target_os = "macos"))]
fn footprint(_pid: u32) -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Pressure;

    pub struct VmStats {
        pub used: u64,
        pub compressed: u64,
        pub swapins: u64,
        pub swapouts: u64,
        pub page: u64,
    }

    #[allow(deprecated)] // libc points to the mach2 crate for these, but they work and save a dependency
    pub fn vm_stats() -> Option<VmStats> {
        // SAFETY: vm_statistics64 holds only integers, for which all zeroes is a value.
        let mut v: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `v` is a vm_statistics64 and `count` holds its size in integers, as host_statistics64 expects.
        let ok = unsafe { libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, (&raw mut v).cast(), &raw mut count) };
        if ok != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: sysconf has no preconditions.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let app = u64::from(v.internal_page_count).saturating_sub(u64::from(v.purgeable_count));
        Some(VmStats {
            used: (app + u64::from(v.wire_count) + u64::from(v.compressor_page_count)) * page,
            compressed: u64::from(v.compressor_page_count) * page,
            swapins: v.swapins,
            swapouts: v.swapouts,
            page,
        })
    }

    /// The level Activity Monitor's memory pressure graph shows.
    pub fn pressure() -> Option<Pressure> {
        let mut level: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        // SAFETY: the name is NUL-terminated and `level`/`size` describe an int, which this sysctl returns.
        let ok = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&raw mut level).cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        match (ok, level) {
            (0, 1) => Some(Pressure::Normal),
            (0, 2) => Some(Pressure::Warning),
            (0, 4) => Some(Pressure::Critical),
            _ => None,
        }
    }
}
