//! The app: one window with the session map, the timeline, token usage and what runs on the machine.
//! Two threads gather the data (the machine every 2 s; sessions, messages and usage from the files Claude Code
//! keeps); the window only ever reads their latest result.
mod dash;
mod glide;
mod icons;
mod layout;
mod map;
mod panel;
mod seg;
mod settings;
mod shell;
mod state;
mod timeline;
mod ui;
mod update;
mod view;

use asterism_core::desktop::DesktopStore;
use asterism_core::graph::{UsageView, Visualizer};
use asterism_core::{Monitor, Report};
use gpui_kit::*;
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
                std::thread::sleep(if usage.ready { SAMPLE_EVERY } else { Duration::from_millis(16) });
            }
        })
        .expect("the sessions thread");

    gpui_kit::application().with_assets(icons::Icons).run(move |cx| {
        gpui_kit::init(cx);
        #[cfg(not(feature = "snapshot"))]
        settings::move_old_data();
        cx.set_global(settings::Settings::load());
        // The inputs and other components follow the window's dark palette, not the system's light one.
        gpui_kit::component::theme::Theme::change(gpui_kit::component::theme::ThemeMode::Dark, None, cx);
        // Snapshot builds can open at another size (MDB_WINDOW=900x700), to check the layout narrow.
        let (w, h) = std::env::var("MDB_WINDOW")
            .ok()
            .filter(|_| cfg!(feature = "snapshot"))
            .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
            .unwrap_or((1320.0, 860.0));
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Asterism".into()), ..Default::default() }),
            window_min_size: Some(size(px(760.), px(520.))),
            ..Default::default()
        };
        // Snapshot runs float almost off-screen and take no focus: another window over them would stop their frames
        // (macOS stops drawing hidden windows), and they stay out of the way of whoever uses the screen.
        #[cfg(feature = "snapshot")]
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(48.) - bounds.size.width, bounds.origin.y), bounds.size))),
            kind: WindowKind::PopUp,
            focus: false,
            ..options
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let machine = cx.new(|cx| view::Machine::new(rx, window, cx));
            cx.new(|cx| shell::Shell::new(machine, grx, update::start(), window, cx))
        })
        .expect("a window");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        #[cfg(not(feature = "snapshot"))]
        cx.activate(true);
    });
}
