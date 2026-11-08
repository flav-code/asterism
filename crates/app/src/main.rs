//! The window: the machine at the top, who uses it on the left, the agent sessions on the right.
mod view;

use gpui_kit::*;
use mdb_core::{Monitor, Report};
use std::sync::Arc;
use std::time::Duration;

const SAMPLE_EVERY: Duration = Duration::from_secs(2);

fn main() {
    // Sampling runs on its own thread; the window only ever reads the latest report.
    let (tx, rx) = tokio::sync::watch::channel(None::<Arc<Report>>);
    std::thread::Builder::new()
        .name("sampler".into())
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
        .expect("the sampler thread");

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("my-dev-business".into()), ..Default::default() }),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| view::Machine::new(rx, window, cx))).expect("a window");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
