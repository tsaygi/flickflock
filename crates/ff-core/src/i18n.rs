//! Localisation.
//!
//! English text is the key: UI code writes `t("Open…")` and gets the string
//! for the active language, falling back to the English key when a
//! translation is missing. Messages with values use numbered placeholders,
//! `tf("Moved {0} to the Trash", &[&name])`, so translations can reorder them.
//!
//! Translations live in `locales/<code>.toml` (a flat `"English" = "…"`
//! table) and are embedded in the binary.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

macro_rules! languages {
    ($( $variant:ident, $code:literal, $native:literal, $file:expr; )*) => {
        /// A supported interface language.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Lang { $( $variant, )* }

        impl Lang {
            pub const ALL: &'static [Lang] = &[ $( Lang::$variant, )* ];

            /// BCP 47 language code used in the config file.
            pub fn code(self) -> &'static str {
                match self { $( Lang::$variant => $code, )* }
            }

            /// The language's own name, for the language picker.
            pub fn native_name(self) -> &'static str {
                match self { $( Lang::$variant => $native, )* }
            }

            fn source(self) -> &'static str {
                match self { $( Lang::$variant => $file, )* }
            }
        }
    };
}

languages! {
    English, "en", "English", "";
    Chinese, "zh-Hans", "简体中文", include_str!("../locales/zh-Hans.toml");
    Hindi, "hi", "हिन्दी", include_str!("../locales/hi.toml");
    Spanish, "es", "Español", include_str!("../locales/es.toml");
    French, "fr", "Français", include_str!("../locales/fr.toml");
    Bengali, "bn", "বাংলা", include_str!("../locales/bn.toml");
    Portuguese, "pt", "Português", include_str!("../locales/pt.toml");
    Russian, "ru", "Русский", include_str!("../locales/ru.toml");
    Indonesian, "id", "Bahasa Indonesia", include_str!("../locales/id.toml");
    German, "de", "Deutsch", include_str!("../locales/de.toml");
    Japanese, "ja", "日本語", include_str!("../locales/ja.toml");
    Turkish, "tr", "Türkçe", include_str!("../locales/tr.toml");
    Korean, "ko", "한국어", include_str!("../locales/ko.toml");
    Vietnamese, "vi", "Tiếng Việt", include_str!("../locales/vi.toml");
    Italian, "it", "Italiano", include_str!("../locales/it.toml");
    Polish, "pl", "Polski", include_str!("../locales/pl.toml");
}

impl Lang {
    /// Exact match on a config value such as `"tr"` or `"zh-Hans"`.
    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL
            .iter()
            .copied()
            .find(|l| l.code().eq_ignore_ascii_case(code))
    }

    /// Best match for an OS locale such as `tr-TR`, `pt_BR.UTF-8` or `zh-CN`.
    pub fn from_locale(locale: &str) -> Option<Lang> {
        let primary = locale
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        Lang::ALL.iter().copied().find(|l| {
            l.code()
                .split('-')
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(&primary)
        })
    }

    /// Resolve the `language` setting: a language code, or `"auto"` to follow
    /// the first OS locale that is supported (English otherwise).
    pub fn resolve(setting: &str, os_locales: &[String]) -> Lang {
        Lang::from_code(setting)
            .or_else(|| os_locales.iter().find_map(|l| Lang::from_locale(l)))
            .unwrap_or(Lang::English)
    }

    fn table(self) -> &'static HashMap<String, String> {
        static TABLES: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();
        let tables =
            TABLES.get_or_init(|| Lang::ALL.iter().map(|l| parse_locale(l.source())).collect());
        &tables[self as usize]
    }

    /// Every translation of this language (empty for English).
    pub fn entries(self) -> &'static HashMap<String, String> {
        self.table()
    }
}

fn parse_locale(source: &str) -> HashMap<String, String> {
    if source.is_empty() {
        return HashMap::new();
    }
    match toml::from_str::<HashMap<String, String>>(source) {
        Ok(table) => table,
        Err(e) => {
            tracing::error!("invalid locale file: {e}");
            HashMap::new()
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Switch the interface language. Takes effect on the next frame.
pub fn set_language(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn language() -> Lang {
    Lang::ALL
        .get(CURRENT.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or(Lang::English)
}

/// Translate `key` (English text) into the active language.
pub fn t(key: &str) -> &str {
    translate(language(), key)
}

/// Translate into a specific language.
pub fn translate(lang: Lang, key: &str) -> &str {
    match lang.table().get(key) {
        Some(found) => found.as_str(),
        None => key,
    }
}

/// Translate `key` and substitute `{0}`, `{1}`, … with `args`.
pub fn tf(key: &str, args: &[&dyn Display]) -> String {
    fill(t(key), args)
}

fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after
            .find('}')
            .and_then(|close| after[..close].parse::<usize>().ok().map(|i| (i, close)))
        {
            Some((index, close)) => {
                if let Some(arg) = args.get(index) {
                    out.push_str(&arg.to_string());
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `{n}` placeholders used in a message, sorted.
pub fn placeholders(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        if let Some(close) = after.find('}') {
            if let Ok(i) = after[..close].parse::<usize>() {
                found.push(i);
            }
        }
        rest = after;
    }
    found.sort_unstable();
    found
}

/// Keys that reach the UI as data rather than as `t("…")` literals in the
/// app: metadata names produced by `meta`, decoder messages, and so on.
pub const DYNAMIC_KEYS: &[&str] = &[
    "File",
    "Image",
    "Name",
    "Folder",
    "Size",
    "Modified",
    "Created",
    "Format",
    "Dimensions",
    "Megapixels",
    "Frames",
    "Color profile",
    "Decoder",
    "Coordinates",
    "HEIC is not supported on this system",
    "AVIF is not supported in this build",
    "Not a supported image format",
    "This RAW file has no embedded preview",
    "Free",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_languages_with_unique_codes() {
        assert_eq!(Lang::ALL.len(), 16);
        let mut codes: Vec<_> = Lang::ALL.iter().map(|l| l.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 16);
        assert!(Lang::ALL.contains(&Lang::Turkish));
        for (i, lang) in Lang::ALL.iter().enumerate() {
            assert_eq!(*lang as usize, i);
            assert!(!lang.native_name().is_empty());
        }
    }

    #[test]
    fn locale_matching() {
        assert_eq!(Lang::from_locale("tr-TR"), Some(Lang::Turkish));
        assert_eq!(Lang::from_locale("pt_BR.UTF-8"), Some(Lang::Portuguese));
        assert_eq!(Lang::from_locale("zh-CN"), Some(Lang::Chinese));
        assert_eq!(Lang::from_locale("ja"), Some(Lang::Japanese));
        assert_eq!(Lang::from_locale("xx-YY"), None);
        assert_eq!(Lang::from_code("zh-hans"), Some(Lang::Chinese));
        assert_eq!(Lang::from_code("auto"), None);
        let os = vec!["sv-SE".to_owned(), "de-DE".to_owned()];
        assert_eq!(Lang::resolve("auto", &os), Lang::German);
        assert_eq!(Lang::resolve("tr", &os), Lang::Turkish);
        assert_eq!(Lang::resolve("auto", &[]), Lang::English);
        assert_eq!(Lang::resolve("nonsense", &["xx".to_owned()]), Lang::English);
    }

    #[test]
    fn placeholders_are_substituted_and_can_be_reordered() {
        assert_eq!(
            fill("Moved {0} to the Trash", &[&"a.jpg"]),
            "Moved a.jpg to the Trash"
        );
        assert_eq!(fill("{1} of {0}", &[&"ten", &3]), "3 of ten");
        assert_eq!(fill("no args {} {x} {9}", &[&1]), "no args {} {x} ");
        assert_eq!(fill("100% {0", &[&1]), "100% {0");
        assert_eq!(placeholders("{1} then {0} and {1}"), [0, 1, 1]);
    }

    #[test]
    fn english_passes_through_and_unknown_keys_fall_back() {
        assert_eq!(translate(Lang::English, "Open…"), "Open…");
        assert_eq!(
            translate(Lang::Turkish, "definitely not a key"),
            "definitely not a key"
        );
    }

    /// Every locale file parses, has no empty values, and keeps the
    /// placeholders of its English key.
    #[test]
    fn locale_files_are_well_formed() {
        for lang in Lang::ALL.iter().filter(|l| **l != Lang::English) {
            let table: HashMap<String, String> =
                toml::from_str(lang.source()).unwrap_or_else(|e| panic!("{}: {e}", lang.code()));
            assert!(!table.is_empty(), "{} has no translations", lang.code());
            for (key, value) in &table {
                assert!(
                    !value.trim().is_empty(),
                    "{}: empty translation for {key:?}",
                    lang.code()
                );
                let (mut want, mut got) = (placeholders(key), placeholders(value));
                want.dedup();
                got.dedup();
                assert_eq!(
                    want,
                    got,
                    "{}: placeholders differ for {key:?} → {value:?}",
                    lang.code()
                );
            }
        }
    }

    #[test]
    fn dynamic_keys_are_translated_everywhere() {
        for lang in Lang::ALL.iter().filter(|l| **l != Lang::English) {
            for key in DYNAMIC_KEYS {
                assert!(
                    lang.entries().contains_key(*key),
                    "{}: missing {key:?}",
                    lang.code()
                );
            }
        }
    }
}
