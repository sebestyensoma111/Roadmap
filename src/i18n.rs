use std::{cell::Cell, collections::HashMap, env, fs, path::PathBuf, sync::OnceLock};

#[cfg(feature = "desktop")]
use std::io;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Hungarian,
    English,
    Spanish,
    German,
}

const LANGUAGES: [Language; 4] = [
    Language::Hungarian, Language::English, Language::Spanish, Language::German,
];
const CODES: [&str; 4] = ["hu", "en", "es", "de"];

thread_local! {
    static CURRENT: Cell<Language> = const { Cell::new(Language::Hungarian) };
}

fn preference_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(PathBuf::from)
            .filter(|path| path.is_absolute()).map(|path| path.join(".config")))
        .map(|path| path.join("roadmap/language"))
}

pub fn load() {
    let index = preference_path().and_then(|path| fs::read_to_string(path).ok())
        .and_then(|code| CODES.iter().position(|&known| known == code.trim()))
        .unwrap_or(0);
    CURRENT.set(LANGUAGES[index]);
}

#[cfg(feature = "desktop")]
pub const NAMES: [&str; 4] = ["Magyar", "English", "Español", "Deutsch"];

#[cfg(feature = "desktop")]
pub fn index() -> usize {
    CURRENT.get() as usize
}

#[cfg(feature = "desktop")]
pub fn select(index: usize) -> io::Result<()> {
    use std::io::Write;

    let language = LANGUAGES[index];
    CURRENT.set(language);
    let path = preference_path().ok_or(io::ErrorKind::NotFound)?;
    fs::create_dir_all(path.parent().unwrap())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let result = writeln!(file, "{}", CODES[index]).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

type Catalog = HashMap<&'static str, [&'static str; 3]>;

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| include_str!("translations.tsv").lines().map(|line| {
        let mut columns = line.split('\t');
        let key = columns.next().unwrap();
        let values = std::array::from_fn(|_| columns.next().unwrap());
        (key, values)
    }).collect())
}

pub fn tr(text: &str) -> &str {
    let language = CURRENT.get() as usize;
    if language == 0 {
        return text;
    }
    catalog().get(text).map_or(text, |values| values[language - 1])
}

pub fn message(key: &str, arguments: &[&str]) -> String {
    let mut parts = tr(key).split("{}");
    let mut result = parts.next().unwrap_or_default().to_string();
    for (index, part) in parts.enumerate() {
        result.push_str(arguments.get(index).copied().unwrap_or("{}"));
        result.push_str(part);
    }
    result
}

pub fn metric<'a>(field: &str, value: &'a str) -> std::borrow::Cow<'a, str> {
    if field == crate::devices::POWER_ON_TIME {
        if let Some(hours) = value.strip_suffix(" óra") {
            return message("{} óra", &[hours]).into();
        }
    }
    if field == "SMART állapot" {
        return tr(value).into();
    }
    value.into()
}

pub fn error(text: &str) -> String {
    for (prefix, key) in [
        ("A grafikus hitelesítés sikertelen. ", "A grafikus hitelesítés sikertelen. {}"),
        ("A smartctl nem adott érvényes JSON-jelentést: ", "A smartctl nem adott érvényes JSON-jelentést: {}"),
    ] {
        if let Some(detail) = text.strip_prefix(prefix) {
            return message(key, &[detail]);
        }
    }
    tr(text).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_four_languages_and_preserves_placeholders() {
        let source = include_str!("translations.tsv");
        assert_eq!(catalog().len(), source.lines().count());
        for line in source.lines() {
            let columns: Vec<_> = line.split('\t').collect();
            assert_eq!(columns.len(), 4, "{line}");
            assert!(columns.iter().all(|part| !part.is_empty()), "{line}");
            let count = columns[0].matches("{}").count();
            assert!(columns.iter().all(|part| part.matches("{}").count() == count), "{line}");
        }
    }

    #[test]
    fn formatting_preserves_device_text() {
        for (language, expected) in [
            (Language::Hungarian, "Mentve · /tmp/{}"),
            (Language::English, "Saved · /tmp/{}"),
            (Language::Spanish, "Guardado · /tmp/{}"),
            (Language::German, "Gespeichert · /tmp/{}"),
        ] {
            CURRENT.set(language);
            assert_eq!(message("Mentve · {}", &["/tmp/{}"]), expected);
        }
    }
}
