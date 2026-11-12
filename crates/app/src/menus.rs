//! Menus the system draws, outside the window. On macOS: the menu bar (the Asterism menu, Window) and the Dock icon's
//! menu. On Windows: the window's own menu (its icon at the top left of the title bar, or Alt+Space) and the taskbar
//! icon's menu. They show About, open the settings, check for updates now and, on macOS, hide, quit and minimize.
use crate::update::Checked;
use gpui_kit::*;
use rust_i18n::t;

gpui_kit::actions!(asterism, [About, Settings, CheckForUpdates, Hide, HideOthers, ShowAll, Quit, Minimize, Zoom]);

const REPO_URL: &str = "https://github.com/flav-code/asterism";

pub fn init(cx: &mut App) {
    cx.on_action(|_: &About, cx| about(cx));
    cx.on_action(|_: &Settings, cx| crate::shell::open_settings(cx));
    cx.on_action(|_: &CheckForUpdates, cx| check_for_updates(cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Minimize, cx| on_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| on_window(cx, |w| w.zoom_window()));
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", Settings, None),
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("cmd-m", Minimize, None),
        ]);
    }
    #[cfg(windows)]
    windows_menu::listen(cx);
}

/// Names the menus in the current language (again after a change).
pub fn set(cx: &mut App) {
    cx.set_menus([
        Menu {
            name: "Asterism".into(),
            items: vec![
                MenuItem::action(t!("menu.about").into_owned(), About),
                MenuItem::action(t!("menu.check").into_owned(), CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action(t!("menu.settings").into_owned(), Settings),
                MenuItem::separator(),
                MenuItem::os_submenu(t!("menu.services").into_owned(), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(t!("menu.hide").into_owned(), Hide),
                MenuItem::action(t!("menu.hide_others").into_owned(), HideOthers),
                MenuItem::action(t!("menu.show_all").into_owned(), ShowAll),
                MenuItem::separator(),
                MenuItem::action(t!("menu.quit").into_owned(), Quit),
            ],
            disabled: false,
        },
        Menu {
            name: t!("menu.window").into_owned().into(),
            items: vec![MenuItem::action(t!("menu.minimize").into_owned(), Minimize), MenuItem::action(t!("menu.zoom").into_owned(), Zoom)],
            disabled: false,
        },
    ]);
    // The Dock icon's menu on macOS, the taskbar icon's on Windows (which starts the app with `--dock-action 0`).
    cx.set_dock_menu(vec![MenuItem::action(t!("menu.check").into_owned(), CheckForUpdates)]);
    #[cfg(windows)]
    windows_menu::name_items();
}

fn on_window(cx: &mut App, f: impl FnOnce(&mut Window)) {
    if let Some(w) = cx.active_window().or_else(|| cx.windows().into_iter().next()) {
        w.update(cx, |_, window, _| f(window)).ok();
    }
}

/// A system dialog over the window, with these buttons; `then` gets the index of the one clicked.
fn tell(cx: &mut App, level: PromptLevel, title: &str, detail: &str, buttons: &[&str], then: impl FnOnce(usize, &mut App) + 'static) {
    // Snapshot builds never put a dialog on whoever's screen: they say it on stderr.
    if cfg!(feature = "snapshot") {
        eprintln!("dialog {level:?}: {title} | {detail} | {buttons:?}");
        return;
    }
    let Some(w) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
    let Ok(answer) = w.update(cx, |_, window, cx| window.prompt(level, title, Some(detail), buttons, cx)) else { return };
    cx.spawn(async move |cx| {
        if let Ok(i) = answer.await {
            cx.update(|cx| then(i, cx));
        }
    })
    .detach();
}

fn about(cx: &mut App) {
    let title = format!("Asterism {}", env!("CARGO_PKG_VERSION"));
    let ok = t!("menu.ok");
    tell(cx, PromptLevel::Info, &title, &t!("menu.about_detail"), &[&ok, "GitHub"], |i, cx| {
        if i == 1 {
            cx.open_url(REPO_URL);
        }
    });
}

/// Checks now, in the settings' updates page: the answer is in the app's own window, not in a system dialog.
fn check_for_updates(cx: &mut App) {
    crate::shell::check_for_updates(cx);
}

/// What a check found, in words.
#[derive(Clone)]
pub struct Said {
    /// How grave it is.
    pub level: PromptLevel,
    pub title: String,
    /// The line under the title.
    pub detail: String,
}

/// What a check found, in words.
pub fn checked(found: Checked) -> Said {
    let version = env!("CARGO_PKG_VERSION");
    let (level, title, detail) = match found {
        Checked::UpToDate => (PromptLevel::Info, t!("update.check.up_to_date", version = version), t!("update.check.up_to_date_detail")),
        Checked::Ready(v) => (PromptLevel::Info, t!("update.check.ready", version = v), t!("update.check.ready_detail")),
        Checked::Scoop => (PromptLevel::Info, t!("update.check.scoop"), t!("update.check.scoop_detail")),
        Checked::NotInstalled => (PromptLevel::Info, t!("update.check.not_installed"), t!("update.check.not_installed_detail")),
        Checked::Off => (PromptLevel::Info, t!("update.check.off"), t!("update.check.off_detail")),
        Checked::Failed(why) => (PromptLevel::Warning, t!("update.check.failed"), why.into()),
    };
    Said { level, title: title.into_owned(), detail: detail.into_owned() }
}

/// The window's own menu on Windows (its icon at the top left of the title bar, or Alt+Space): Windows draws it, and
/// takes two more items; their clicks come back as WM_SYSCOMMAND, caught before GPUI's handling.
#[cfg(windows)]
pub mod windows_menu {
    use gpui_kit::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use rust_i18n::t;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, FindWindowW, GetSystemMenu, MF_BYCOMMAND, MF_SEPARATOR, MF_STRING, ModifyMenuW, PostMessageW, WM_SYSCOMMAND,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    /// Command ids: below 0xF000 (the system's) and with the low four bits clear, as Windows wants.
    pub const CHECK: usize = 0x1010;
    pub const ABOUT: usize = 0x1020;
    pub const SETTINGS: usize = 0x1030;

    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static CLICKS: OnceLock<tokio::sync::mpsc::UnboundedSender<usize>> = OnceLock::new();

    /// Runs the items' clicks as the app's actions.
    pub fn listen(cx: &mut App) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        CLICKS.set(tx).ok();
        cx.spawn(async move |cx| {
            while let Some(id) = rx.recv().await {
                cx.update(|cx| match id {
                    CHECK => super::check_for_updates(cx),
                    SETTINGS => crate::shell::open_settings(cx),
                    _ => super::about(cx),
                });
            }
        })
        .detach();
    }

    fn hwnd(window: &Window) -> Option<HWND> {
        // The trait's method: GPUI's own `window_handle` has the same name.
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as _)),
            _ => None,
        }
    }

    /// Adds the items to the window's menu and catches their clicks.
    pub fn attach(window: &Window) {
        let Some(hwnd) = hwnd(window) else { return };
        WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
        // SAFETY: `hwnd` is this window's handle, alive while GPUI holds the window; the strings outlive each call;
        // `on_message` has the signature a subclass procedure must have.
        unsafe {
            let menu = GetSystemMenu(hwnd, false);
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok();
            AppendMenuW(menu, MF_STRING, CHECK, &HSTRING::from(&*t!("menu.check"))).ok();
            AppendMenuW(menu, MF_STRING, SETTINGS, &HSTRING::from(&*t!("menu.settings"))).ok();
            AppendMenuW(menu, MF_STRING, ABOUT, &HSTRING::from(&*t!("menu.about"))).ok();
            let _ = SetWindowSubclass(hwnd, Some(on_message), 1, 0);
        }
    }

    /// The items in the current language.
    pub fn name_items() {
        let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as _);
        if hwnd.0.is_null() {
            return;
        }
        // SAFETY: `hwnd` was stored by `attach` from the live window, and Windows checks the handle itself: on a
        // closed window these calls fail, which is ignored. The strings outlive each call.
        unsafe {
            let menu = GetSystemMenu(hwnd, false);
            ModifyMenuW(menu, CHECK as u32, MF_BYCOMMAND | MF_STRING, CHECK, &HSTRING::from(&*t!("menu.check"))).ok();
            ModifyMenuW(menu, SETTINGS as u32, MF_BYCOMMAND | MF_STRING, SETTINGS, &HSTRING::from(&*t!("menu.settings"))).ok();
            ModifyMenuW(menu, ABOUT as u32, MF_BYCOMMAND | MF_STRING, ABOUT, &HSTRING::from(&*t!("menu.about"))).ok();
        }
    }

    /// The window's subclass procedure.
    ///
    /// # Safety
    /// Only Windows calls it, as the procedure `attach` registered: `hwnd` is that window.
    unsafe extern "system" fn on_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
        let id = wparam.0 & 0xfff0;
        if msg == WM_SYSCOMMAND && (id == CHECK || id == ABOUT || id == SETTINGS) {
            if let Some(tx) = CLICKS.get() {
                tx.send(id).ok();
            }
            return LRESULT(0);
        }
        // SAFETY: the arguments are the ones Windows gave this procedure, passed on unchanged.
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// Started from the taskbar icon's menu (`--dock-action <n>`) while Asterism already runs: the running window gets
    /// the command, as if its own menu had been clicked. False when no Asterism window is open.
    pub fn forward(action: usize) -> bool {
        // SAFETY: both names are NUL-terminated constants.
        let Ok(hwnd) = (unsafe { FindWindowW(w!("Zed::Window"), w!("Asterism")) }) else { return false };
        let id = if action == 0 { CHECK } else { ABOUT };
        // SAFETY: the message carries two integers and no pointer; Windows checks `hwnd` and fails if it is gone.
        unsafe { PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(id), LPARAM(0)).is_ok() }
    }
}
