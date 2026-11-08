//! The app: one window with the session map, the timeline, token usage and what runs on the machine.
//! Two threads gather the data (the machine every 2 s; sessions, messages and usage from the files Claude Code
//! keeps); the window only ever reads their latest result.
mod dash;
mod glide;
mod icons;
mod layout;
mod map;
mod panel;
mod settings;
mod shell;
mod state;
mod timeline;
mod ui;
mod view;

use gpui_kit::*;
use mdb_core::desktop::DesktopStore;
use mdb_core::graph::{UsageView, Visualizer};
use mdb_core::{Monitor, Report};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

const SAMPLE_EVERY: Duration = Duration::from_secs(2);

fn main() {
    let (tx, rx) = tokio::sync::watch::channel(None::<Arc<Report>>);
    std::thread::Builder::new()
        .name("machine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime");
            rt.block_on(async move {
                let mut monitor = Monitor::default();
                monitor.sample().await; // only sets the CPU baseline: the first report comes half a second later
                let mut wait = Duration::from_millis(500);
                loop {
                    tokio::time::sleep(wait).await;
                    if tx.send(Some(Arc::new(monitor.sample().await))).is_err() {
                        break;
                    }
                    wait = SAMPLE_EVERY;
                }
            });
        })
        .expect("the machine thread");

    let (gtx, grx) = tokio::sync::watch::channel(None);
    std::thread::Builder::new()
        .name("sessions".into())
        .spawn(move || {
            #[allow(deprecated)]
            let home = std::env::home_dir().unwrap_or_default();
            let (mut store, mut viz) = (DesktopStore::default(), Visualizer::default());
            let mut sys = System::new().expect("sysinfo supports this OS");
            let (mut usage, mut usage_version) = (Arc::new(UsageView::default()), u64::MAX);
            loop {
                sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
                let live: HashSet<u32> = sys.processes().keys().map(|p| p.as_u32()).collect();
                let desktop = store.refresh(&home);
                let graph = viz.build(&home, &desktop, &live, ui::now_ms());
                if viz.usage.version != usage_version {
                    (usage, usage_version) = (Arc::new(viz.usage_view()), viz.usage.version);
                }
                if gtx.send(Some((Arc::new(graph), usage.clone()))).is_err() {
                    break;
                }
                // The history is read in steps right after launch, then the files are checked every 2 s.
                std::thread::sleep(if usage.ready { SAMPLE_EVERY } else { Duration::from_millis(50) });
            }
        })
        .expect("the sessions thread");

    gpui_kit::application().with_assets(icons::Icons).run(move |cx| {
        gpui_kit::init(cx);
        cx.set_global(settings::Settings::load());
        // The inputs and other components follow the window's dark palette, not the system's light one.
        gpui_kit::component::theme::Theme::change(gpui_kit::component::theme::ThemeMode::Dark, None, cx);
        let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("my-dev-business".into()), ..Default::default() }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let machine = cx.new(|cx| view::Machine::new(rx, window, cx));
            cx.new(|cx| shell::Shell::new(machine, grx, window, cx))
        })
        .expect("a window");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
