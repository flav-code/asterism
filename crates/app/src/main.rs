//! The app: one window with the session map, the timeline, token usage and what runs on the machine.
//! Two threads gather the data (the machine every 2 s; sessions, messages and usage from the files Claude Code
//! keeps, with those of other machines over SSH); the window only ever reads their latest result.
// Windows: a GUI app, or Windows opens an empty console next to the window (and closing it quits the app).
#![cfg_attr(windows, windows_subsystem = "windows")]
mod dash;
#[cfg(feature = "snapshot")]
mod demo;
mod glide;
mod icons;
mod layout;
mod machine_view;
mod machines;
mod map;
mod menus;
mod panel;
mod seg;
mod settings;
mod shell;
mod state;
mod timeline;
mod ui;
mod update;

rust_i18n::i18n!("../../locales", fallback = "en");

use asterism_core::{Monitor, Report};
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::*;
use std::sync::Arc;
use std::time::Duration;

const SAMPLE_EVERY: Duration = Duration::from_secs(2);

fn main() {
    // Started from the taskbar icon's menu (`--dock-action <n>`): an Asterism already open does it, and this one stops.
    let dock_action = std::env::args().skip_while(|a| a != "--dock-action").nth(1).and_then(|n| n.parse::<usize>().ok());
    #[cfg(windows)]
    if dock_action.is_some_and(menus::windows_menu::forward) {
        return;
    }
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
            // What was read of the history is kept next to the settings: the next launch reads only what is new.
            // Snapshot builds keep nothing, unless MDB_CACHE names a file.
            let cache = if cfg!(feature = "snapshot") {
                std::env::var_os("MDB_CACHE").map(std::path::PathBuf::from)
            } else {
                Some(settings::history_file())
            };
            asterism_core::graph::follow(&home, cache.as_deref(), |graph, usage| gtx.send(Some(machines::merged(graph, usage))).is_ok());
        })
        .expect("the sessions thread");

    gpui_kit::application().with_assets(icons::Icons).run(move |cx| {
        gpui_kit::init(cx);
        #[cfg(not(feature = "snapshot"))]
        settings::move_old_data();
        cx.set_global(settings::Settings::load());
        menus::init(cx);
        settings::apply_language(cx);
        // Snapshot builds: MDB_MENUS=1 prints the menus set, to check them without a screen.
        #[cfg(feature = "snapshot")]
        if std::env::var("MDB_MENUS").is_ok() {
            for m in cx.get_menus().unwrap_or_default() {
                let items: Vec<String> = m
                    .items
                    .iter()
                    .map(|i| match i {
                        gpui_kit::OwnedMenuItem::Action { name, action, .. } => format!("{name} ({})", action.name()),
                        gpui_kit::OwnedMenuItem::SystemMenu(m) => format!("[{}]", m.name),
                        gpui_kit::OwnedMenuItem::Separator => "—".into(),
                        gpui_kit::OwnedMenuItem::Submenu(m) => format!("{} ▸", m.name),
                    })
                    .collect();
                eprintln!("menu {}: {}", m.name, items.join(" | "));
            }
        }
        // The inputs and other components follow the window's dark palette, not the system's light one.
        Theme::change(ThemeMode::Dark, None, cx);
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
            #[cfg(windows)]
            menus::windows_menu::attach(window);
            let machine = cx.new(|cx| machine_view::MachineView::new(rx, window, cx));
            let shell = cx.new(|cx| shell::Shell::new(machine, grx, update::start(), window, cx));
            cx.set_global(shell::ShellHandle(shell.downgrade()));
            shell
        })
        .expect("a window");
        // Not forwarded: nothing was open, so this one does it once its window is there.
        if let Some(n) = dock_action {
            cx.perform_dock_menu_action(n);
        }
        // Snapshot builds: MDB_ACTION=check|about|settings runs that menu item.
        #[cfg(feature = "snapshot")]
        match std::env::var("MDB_ACTION").as_deref() {
            Ok("check") => cx.dispatch_action(&menus::CheckForUpdates),
            Ok("about") => cx.dispatch_action(&menus::About),
            Ok("settings") => shell::open_settings(cx),
            _ => {}
        }
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
