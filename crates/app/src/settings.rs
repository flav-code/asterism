//! Choices that survive a restart: the tab, range and filters, map titles, the side panel and the feed, the usage
//! range and metric. Saved as JSON next to the map's positions. Snapshot builds neither read nor write them.
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
            return d;
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
        }
    }

    fn save(&self) {
        if cfg!(feature = "snapshot") {
            return;
        }
        let v = json!({
            "tab": self.tab, "range": self.range, "project": self.project, "archived": self.archived, "agents": self.agents,
            "map_titles": self.titles, "panel": self.panel, "feed": self.feed,
            "usage_range": self.usage_range, "usage_metric": self.usage_metric,
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
