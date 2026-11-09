//! Choices that survive a restart: the tab, range and filters, map titles, the side panel and the feed, the usage
//! range and metric, the language, the other machines. Saved as JSON next to the map's positions. Snapshot builds neither read nor write
//! them.
use asterism_core::i18n;
use gpui_kit::*;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub tab: String,
    pub range: String,
    pub project: Option<String>,
    pub archived: bool,
    pub agents: bool,
    pub titles: String,
    pub panel: bool,
    pub feed: bool,
    pub usage_range: String,
    pub usage_metric: String,
    /// A code from `i18n::LANGUAGES`; none follows the system's language.
    pub language: Option<String>,
    /// Other machines whose sessions show too, reached over SSH (see `machines`).
    pub machines: Vec<Machine>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub name: String,
    /// What follows `ssh` in a terminal: `me@host`, or a Host from ~/.ssh/config.
    pub target: String,
}

impl Global for Settings {}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tab: "map".into(),
            range: "24h".into(),
            project: None,
            archived: true,
            agents: true,
            titles: "hover".into(),
            panel: true,
            feed: true,
            usage_range: "30d".into(),
            usage_metric: "usd".into(),
            language: None,
            machines: Vec::new(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Asterism")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map_or(home.clone(), PathBuf::from).join("Asterism")
    } else {
        home.join(".config/asterism")
    }
}

/// The app was called my-dev-business: its settings and saved map move to the new folder once.
#[cfg(not(feature = "snapshot"))]
pub fn move_old_data() {
    let new = data_dir();
    let old = new.with_file_name("my-dev-business");
    if !new.exists() && old.is_dir() {
        std::fs::rename(&old, &new).ok();
    }
}

impl Settings {
    pub fn load() -> Self {
        let d = Settings::default();
        if cfg!(feature = "snapshot") {
            // Snapshot builds can be given machines: MDB_MACHINES="name=target;…".
            let machines = std::env::var("MDB_MACHINES").unwrap_or_default();
            let machines = machines
                .split(';')
                .filter_map(|m| m.split_once('='))
                .map(|(name, target)| Machine { name: name.into(), target: target.into() })
                .collect();
            return Settings { machines, ..d };
        }
        let Some(v) = std::fs::read(data_dir().join("settings.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
            return d;
        };
        let s = |k: &str, or: &str| v.get(k).and_then(Value::as_str).unwrap_or(or).to_string();
        let b = |k: &str, or: bool| v.get(k).and_then(Value::as_bool).unwrap_or(or);
        Settings {
            tab: s("tab", &d.tab),
            range: s("range", &d.range),
            project: v.get("project").and_then(Value::as_str).map(str::to_string),
            archived: b("archived", d.archived),
            agents: b("agents", d.agents),
            titles: s("map_titles", &d.titles),
            panel: b("panel", d.panel),
            feed: b("feed", d.feed),
            usage_range: s("usage_range", &d.usage_range),
            usage_metric: s("usage_metric", &d.usage_metric),
            language: v.get("language").and_then(Value::as_str).filter(|l| i18n::LANGUAGES.iter().any(|(c, _)| c == l)).map(str::to_string),
            machines: v
                .get("machines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|m| Some(Machine { name: m.get("name")?.as_str()?.into(), target: m.get("target")?.as_str()?.into() }))
                .filter(|m| crate::machines::check(&m.name, &m.target, &[]).is_ok())
                .collect(),
        }
    }

    fn save(&self) {
        if cfg!(feature = "snapshot") {
            return;
        }
        let v = json!({
            "tab": self.tab, "range": self.range, "project": self.project, "archived": self.archived, "agents": self.agents,
            "map_titles": self.titles, "panel": self.panel, "feed": self.feed,
            "usage_range": self.usage_range, "usage_metric": self.usage_metric, "language": self.language,
            "machines": self.machines.iter().map(|m| json!({ "name": m.name, "target": m.target })).collect::<Vec<_>>(),
        });
        let dir = data_dir();
        // A lost setting is only a default next time: no need to bother anyone with an error.
        if std::fs::create_dir_all(&dir).is_ok() {
            std::fs::write(dir.join("settings.json"), v.to_string()).ok();
        }
    }
}

/// Changes the settings and saves them.
pub fn change(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    let s = cx.global_mut::<Settings>();
    let before = s.clone();
    f(s);
    if *s != before {
        s.save();
    }
}

/// The language the system asks for, among those there is a file for (English if none). Snapshot builds take
/// MDB_LANG instead, English by default, so their pictures don't depend on the machine.
pub fn system_language() -> &'static str {
    if cfg!(feature = "snapshot") {
        return i18n::pick(std::env::var("MDB_LANG").ok().as_deref());
    }
    let wanted: Vec<String> = sys_locale::get_locales().collect();
    i18n::pick(wanted.iter().map(String::as_str))
}

/// Shows the window in the chosen language (or the system's), from now on and at the next launch.
pub fn set_language(cx: &mut App, language: Option<String>) {
    change(cx, |s| s.language = language);
    apply_language(cx);
}

/// Sets the language the settings ask for and draws the windows again in it.
pub fn apply_language(cx: &mut App) {
    let lang = cx.global::<Settings>().language.clone();
    i18n::set(lang.as_deref().unwrap_or_else(|| system_language()));
    // The other machines' agents name things in this language too.
    crate::machines::sync(&cx.global::<Settings>().machines, &i18n::current());
    cx.refresh_windows();
}

/// Adds or removes other machines, and connects to them or lets them go.
pub fn set_machines(cx: &mut App, machines: Vec<Machine>) {
    change(cx, |s| s.machines = machines);
    crate::machines::sync(&cx.global::<Settings>().machines, &i18n::current());
}
