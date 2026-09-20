//! `[[bugün]]`-style links count as links to the daily note: they give it a backlink (so it is not
//! an orphan), and "today" is a date the caller chooses, not the wall clock.

use chrono::NaiveDate;
use satz_core::config::DailyNoteConfig;
use satz_core::{DocId, Index, parse_document};
use std::path::Path;

fn doc(text: &str, path: &str) -> satz_core::Document {
    parse_document(text, Path::new(path))
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn backlinks(index: &Index, path: &str) -> Vec<String> {
    let mut v: Vec<String> = index
        .backlinks_of(&DocId::new(path))
        .map(|id| id.as_str().to_string())
        .collect();
    v.sort();
    v
}

fn orphans(index: &Index) -> Vec<String> {
    let mut v: Vec<String> = index
        .orphan_docs()
        .map(|d| d.path.to_string_lossy().replace('\\', "/"))
        .collect();
    v.sort();
    v
}

fn vault() -> Index {
    Index::build(vec![
        doc("# Log\n\n[[bugün]] and [[dün]] and [[yarın]]\n", "log.md"),
        doc("# Mon\n", "daily/2026-03-13.md"),
        doc("# Tue\n", "daily/2026-03-14.md"),
        doc("# Wed\n", "daily/2026-03-15.md"),
    ])
}

#[test]
fn without_a_daily_setting_alias_links_give_no_backlinks() {
    let index = vault();
    assert!(backlinks(&index, "daily/2026-03-14.md").is_empty());
    assert!(orphans(&index).contains(&"daily/2026-03-14.md".to_string()));
}

#[test]
fn today_yesterday_and_tomorrow_links_reach_the_right_notes() {
    let mut index = vault();
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    assert_eq!(backlinks(&index, "daily/2026-03-14.md"), vec!["log.md"]);
    assert_eq!(backlinks(&index, "daily/2026-03-13.md"), vec!["log.md"]);
    assert_eq!(backlinks(&index, "daily/2026-03-15.md"), vec!["log.md"]);
    assert_eq!(
        orphans(&index),
        vec!["log.md"],
        "only the note nobody links to is an orphan"
    );
}

#[test]
fn the_date_is_the_one_given_not_the_clock() {
    let mut index = vault();
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 15))));
    // today = 15th: today -> 15, yesterday -> 14, tomorrow -> 16 (does not exist)
    assert_eq!(backlinks(&index, "daily/2026-03-15.md"), vec!["log.md"]);
    assert_eq!(backlinks(&index, "daily/2026-03-14.md"), vec!["log.md"]);
    assert!(backlinks(&index, "daily/2026-03-13.md").is_empty());
}

#[test]
fn moving_the_date_moves_the_backlinks() {
    let mut index = vault();
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 15))));
    assert!(backlinks(&index, "daily/2026-03-13.md").is_empty());
    index.set_daily(None);
    assert!(backlinks(&index, "daily/2026-03-14.md").is_empty());
}

#[test]
fn a_missing_day_stays_a_broken_link_and_makes_no_backlink() {
    let mut index = Index::build(vec![doc("# Log\n\n[[bugün]]\n", "log.md")]);
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    assert!(backlinks(&index, "log.md").is_empty());
    assert_eq!(
        orphans(&index),
        vec!["log.md"],
        "nothing links to log.md, and no daily note appeared"
    );
}

#[test]
fn a_custom_format_and_folder_are_honoured() {
    let cfg = DailyNoteConfig {
        folder: "journal/".to_string(),
        format: "%d.%m.%Y".to_string(),
        ..Default::default()
    };
    let mut index = Index::build(vec![
        doc("# Log\n\n[[today]]\n", "log.md"),
        doc("# D\n", "journal/14.03.2026.md"),
        doc("# Other\n", "daily/2026-03-14.md"),
    ]);
    index.set_daily(Some((cfg, day(2026, 3, 14))));
    assert_eq!(backlinks(&index, "journal/14.03.2026.md"), vec!["log.md"]);
    assert!(backlinks(&index, "daily/2026-03-14.md").is_empty());
}

#[test]
fn a_real_note_with_the_alias_name_wins_over_the_daily_meaning() {
    let mut index = Index::build(vec![
        doc("# Log\n\n[[today]]\n", "log.md"),
        doc("# T\n", "today.md"),
        doc("# D\n", "daily/2026-03-14.md"),
    ]);
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    assert_eq!(backlinks(&index, "today.md"), vec!["log.md"]);
    assert!(backlinks(&index, "daily/2026-03-14.md").is_empty());
}

#[test]
fn edits_and_removals_keep_the_alias_backlinks_exact() {
    let mut index = vault();
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    index.replace_doc(doc("# Log\n\n[[bugün]]\n", "log.md")); // incremental edit
    assert_eq!(backlinks(&index, "daily/2026-03-14.md"), vec!["log.md"]);
    assert!(backlinks(&index, "daily/2026-03-13.md").is_empty());
    index.remove_doc(&DocId::new("log.md"));
    assert!(backlinks(&index, "daily/2026-03-14.md").is_empty());
}

#[test]
fn a_daily_note_created_later_gets_its_backlink() {
    let mut index = Index::build(vec![doc("# Log\n\n[[bugün]]\n", "log.md")]);
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    index.replace_doc(doc("# New\n", "daily/2026-03-14.md"));
    assert_eq!(backlinks(&index, "daily/2026-03-14.md"), vec!["log.md"]);
}

#[test]
fn setting_the_same_daily_twice_does_not_touch_the_index() {
    let mut index = vault();
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    let revision = index.revision();
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 14))));
    assert_eq!(index.revision(), revision);
    index.set_daily(Some((DailyNoteConfig::default(), day(2026, 3, 15))));
    assert_ne!(index.revision(), revision);
}

#[test]
fn resolve_relative_daily_on_takes_the_date() {
    let index = vault();
    let cfg = DailyNoteConfig::default();
    let id = index.resolve_relative_daily_on("Bugün", &cfg, day(2026, 3, 13));
    assert_eq!(id.map(|i| i.as_str()), Some("daily/2026-03-13.md"));
    assert!(
        index
            .resolve_relative_daily_on("nonsense", &cfg, day(2026, 3, 13))
            .is_none()
    );
    assert!(
        index
            .resolve_relative_daily_on("bugün", &cfg, day(2001, 1, 1))
            .is_none()
    );
}
