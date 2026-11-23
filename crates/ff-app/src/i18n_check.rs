//! Keeps the translations honest: every string the UI can show must exist
//! in every locale file, and locale files must not carry stale keys.
#![cfg(test)]

use ff_core::edit::SaveFormat;
use ff_core::folder::{GroupKey, SortKey};
use ff_core::i18n::{Lang, DYNAMIC_KEYS};
use ff_core::input::Action;
use std::collections::BTreeSet;

/// Extract the literals of `t("…")` and `tf("…", …)` calls from Rust source.
fn scan(source: &str, keys: &mut BTreeSet<String>) {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while let Some(found) = source[i..].find('(') {
        let open = i + found;
        i = open + 1;
        // The callee must be exactly `t` or `tf` (not `.text(`, `format(`, `x.t(`).
        let before = &source[..open];
        let name_start = before
            .rfind(|c: char| !is_ident(c))
            .map(|p| p + 1)
            .unwrap_or(0);
        if !matches!(&before[name_start..], "t" | "tf") || before[..name_start].ends_with('.') {
            continue;
        }
        // rustfmt may put the literal on the next line.
        let rest = source[open + 1..].trim_start();
        let Some(literal) = rest.strip_prefix('"') else {
            continue;
        };
        let mut key = String::new();
        let mut chars = literal.chars();
        loop {
            match chars.next() {
                Some('\\') => match chars.next() {
                    Some('n') => key.push('\n'),
                    Some(other) => key.push(other),
                    None => break,
                },
                Some('"') | None => break,
                Some(c) => key.push(c),
            }
        }
        keys.insert(key);
    }
}

fn all_keys() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).expect("src dir").flatten() {
        let path = entry.path();
        let is_this_file = path
            .file_name()
            .map(|n| n == "i18n_check.rs")
            .unwrap_or(false);
        if path.extension().map(|e| e == "rs").unwrap_or(false) && !is_this_file {
            scan(
                &std::fs::read_to_string(&path).expect("readable source"),
                &mut keys,
            );
        }
    }
    keys.extend(Action::ALL.iter().map(|a| a.label().to_owned()));
    keys.extend(SortKey::ALL.iter().map(|k| k.label().to_owned()));
    keys.extend(GroupKey::ALL.iter().map(|k| k.label().to_owned()));
    keys.extend(SaveFormat::ALL.iter().map(|f| f.label().to_owned()));
    keys.extend(DYNAMIC_KEYS.iter().map(|k| (*k).to_owned()));
    keys
}

#[test]
fn scanner_finds_calls() {
    let mut keys = BTreeSet::new();
    scan(
        r#"ui.button(t("Open…")); x.t("no"); tf("Saved {0}", &[&n]); format("nope"); t("a \"b\"")"#,
        &mut keys,
    );
    let found: Vec<&str> = keys.iter().map(String::as_str).collect();
    assert_eq!(found, ["Open…", "Saved {0}", "a \"b\""]);
}

#[test]
fn every_language_translates_every_string() {
    let keys = all_keys();
    assert!(
        keys.len() > 150,
        "the scanner found suspiciously few strings ({})",
        keys.len()
    );
    for lang in Lang::ALL.iter().filter(|l| **l != Lang::English) {
        let table = lang.entries();
        let missing: Vec<&String> = keys.iter().filter(|k| !table.contains_key(*k)).collect();
        assert!(
            missing.is_empty(),
            "{}: {} missing translation(s), e.g. {:?}",
            lang.code(),
            missing.len(),
            &missing[..missing.len().min(8)]
        );
        let stale: Vec<&String> = table.keys().filter(|k| !keys.contains(*k)).collect();
        assert!(
            stale.is_empty(),
            "{}: {} unused key(s), e.g. {:?}",
            lang.code(),
            stale.len(),
            &stale[..stale.len().min(8)]
        );
    }
}

/// `FLICKFLOCK_DUMP_KEYS=<file> cargo test -p flickflock dump_keys -- --ignored`
/// writes the master list of strings, one TOML line per key, for translators.
#[test]
#[ignore]
fn dump_keys() {
    let Some(path) = std::env::var_os("FLICKFLOCK_DUMP_KEYS") else {
        return;
    };
    let mut out = String::new();
    for key in all_keys() {
        let quoted = toml::Value::String(key).to_string();
        out.push_str(&format!("{quoted} = {quoted}\n"));
    }
    std::fs::write(path, out).expect("write key list");
}
