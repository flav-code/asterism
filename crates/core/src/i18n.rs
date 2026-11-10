//! Translations: one JSON file per language in `locales/` at the repo root, `en.json` being the reference. Each crate
//! that shows text calls `rust_i18n::i18n!` on that folder and writes `t!` with a key. The window picks the language; the
//! CLI never does, so it stays in English.
use rust_i18n::t;

/// A language there is a file for.
#[derive(Clone, Copy)]
pub struct Language {
    pub code: &'static str,
    /// Its name in itself.
    pub name: &'static str,
}

pub const LANGUAGES: [Language; 2] = [Language { code: "en", name: "English" }, Language { code: "fr", name: "Français" }];

/// The first of these system languages ("fr-CA", "en_US", "fr"…) there is a file for; English if none.
pub fn pick<'a>(wanted: impl IntoIterator<Item = &'a str>) -> &'static str {
    wanted
        .into_iter()
        .find_map(|w| {
            let base = w.split(['-', '_', '.']).next().unwrap_or("").to_ascii_lowercase();
            LANGUAGES.iter().find(|l| l.code == base).map(|l| l.code)
        })
        .unwrap_or("en")
}

pub fn set(lang: &str) {
    rust_i18n::set_locale(lang);
}

pub fn current() -> String {
    rust_i18n::locale().to_string()
}

/// The name of a language in itself ("Français"), or the code when there is no file for it.
pub fn name(code: &str) -> &str {
    LANGUAGES.iter().find(|l| l.code == code).map_or(code, |l| l.name)
}

/// `key.one` or `key.other` for `n`, with `%{n}` filled in. French says 0 and 1 in the singular, English only 1.
pub fn count(key: &str, n: u64) -> String {
    let one = if current() == "fr" { n < 2 } else { n == 1 };
    t!(format!("{key}.{}", if one { "one" } else { "other" }), n = n).into_owned()
}

/// `x` with `places` decimals and this language's decimal mark ("1.5" in English, "1,5" in French).
pub fn decimal(x: f64, places: usize) -> String {
    let s = format!("{x:.places$}");
    let mark = t!("number.decimal");
    if mark == "." { s } else { s.replace('.', &mark) }
}

/// The chrono `pattern` with its day and month names (`%a`, `%b`) written in this language for `d`, since chrono
/// only knows English ones: `d.format(&i18n::names(&d, "%a %-d %b"))`.
pub fn names(d: &impl chrono::Datelike, pattern: &str) -> String {
    let pick = |list: &str, i: usize| list.split(' ').nth(i).unwrap_or("").to_string();
    pattern
        .replace("%a", &pick(&t!("date.days_from_sunday"), d.weekday().num_days_from_sunday() as usize))
        .replace("%b", &pick(&t!("date.months"), d.month0() as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap()
    }

    /// Every key of a language file, flattened ("map.empty"), with its text.
    fn keys(lang: &str) -> BTreeMap<String, String> {
        fn walk(prefix: &str, v: &Value, out: &mut BTreeMap<String, String>) {
            match v {
                Value::Object(o) => {
                    o.iter().for_each(|(k, v)| walk(&if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, v, out))
                }
                Value::String(s) => drop(out.insert(prefix.to_string(), s.clone())),
                other => panic!("{prefix} must be text, not {other}"),
            }
        }
        // Like rust-i18n: every file under locales/ named `<lang>.json` or `<anything>.<lang>.json`.
        let mut out = BTreeMap::new();
        let mut dirs = vec![root().join("locales")];
        while let Some(dir) = dirs.pop() {
            for p in std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()) {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "json") && stem.rsplit('.').next() == Some(lang) {
                    walk("", &serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap(), &mut out);
                }
            }
        }
        out
    }

    fn placeholders(s: &str) -> BTreeSet<&str> {
        s.match_indices("%{").filter_map(|(i, _)| s[i..].find('}').map(|j| &s[i + 2..i + j])).collect()
    }

    #[test]
    fn every_language_has_every_key_with_the_same_placeholders() {
        let en = keys("en");
        let others = LANGUAGES.iter().map(|l| l.code).filter(|code| *code != "en");
        for lang in others {
            let other = keys(lang);
            let missing: Vec<_> = en.keys().filter(|k| !other.contains_key(*k)).collect();
            let extra: Vec<_> = other.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(missing.is_empty() && extra.is_empty(), "{lang}.json: missing {missing:?}, not in en.json {extra:?}");
            for (k, text) in &en {
                assert_eq!(placeholders(text), placeholders(&other[k]), "{lang}.json: {k} has other %{{…}} than en.json");
            }
        }
        for Language { code: lang, .. } in LANGUAGES {
            let k = keys(lang);
            assert_eq!(k["date.days_from_sunday"].split(' ').count(), 7, "{lang}.json: 7 days");
            assert_eq!(k["date.months"].split(' ').count(), 12, "{lang}.json: 12 months");
        }
    }

    /// Each key the code names with `t!` or `count` exists, so no raw key ever shows in the window.
    #[test]
    fn every_key_in_the_code_exists() {
        let en = keys("en");
        let mut missing = Vec::new();
        let mut files = vec![root().join("crates")];
        while let Some(p) = files.pop() {
            if p.is_dir() {
                files.extend(std::fs::read_dir(&p).unwrap().flatten().map(|e| e.path()));
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            for (call, plural) in [("t!(\"", false), ("count(\"", true)] {
                for (i, _) in src.match_indices(call) {
                    let rest = &src[i + call.len()..];
                    let key = &rest[..rest.find('"').unwrap()];
                    let found = if plural {
                        en.contains_key(&format!("{key}.one")) && en.contains_key(&format!("{key}.other"))
                    } else {
                        en.contains_key(key)
                    };
                    if !found && !key.contains('{') {
                        missing.push(format!("{}: {key}", p.strip_prefix(root()).unwrap().display()));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "not in en.json: {missing:#?}");
    }

    #[test]
    fn helpers() {
        assert_eq!(pick(["de-DE", "fr-CA"]), "fr");
        assert_eq!(pick(["en_US.UTF-8"]), "en");
        assert_eq!(pick(Vec::<&str>::new()), "en");
        let d = chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        // One test sets the language, so the others never race on it.
        set("fr");
        assert_eq!(decimal(1.5, 1), "1,5");
        assert_eq!(d.format(&names(&d, "%a %-d %b")).to_string(), "dim. 8 nov.");
        assert_eq!(count("shell.stats.sessions", 0), "session");
        set("en");
        assert_eq!(decimal(1.5, 1), "1.5");
        assert_eq!(d.format(&names(&d, "%a %-d %b")).to_string(), "Sun 8 Nov");
        assert_eq!(count("shell.stats.sessions", 0), "sessions");
        assert_eq!(count("shell.stats.sessions", 1), "session");
    }
}
