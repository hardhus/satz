use satz_core::{Index, parse_document};
use std::path::Path;

#[test]
fn test_index_from_fixtures() {
    let docs = vec![
        parse_document(
            include_str!("fixtures/daily_note.md"),
            Path::new("daily_note.md"),
        ),
        parse_document(
            include_str!("fixtures/book_note.md"),
            Path::new("book_note.md"),
        ),
        parse_document(
            include_str!("fixtures/tractatus_style.md"),
            Path::new("tractatus/1.1.md"),
        ),
        parse_document(
            include_str!("fixtures/edge_case.md"),
            Path::new("tests/edge_case.md"),
        ),
    ];
    let index = Index::build(docs);

    assert_eq!(index.doc_count(), 4);

    // book_note aliases: TLP, Tractatus
    assert!(index.resolve_link("TLP").is_some());
    assert!(index.resolve_link("Tractatus").is_some());

    // tractatus/1.1.md alias: "1.1"
    assert!(index.resolve_link("1.1").is_some());

    // Tags are indexed
    assert!(index.docs_with_tag("felsefe").count() > 0);
    assert!(index.docs_with_tag("daily").count() > 0);

    // Stats
    let stats = index.stats();
    assert_eq!(stats.doc_count, 4);
    assert!(stats.unique_tags > 0);
    assert!(stats.total_links > 0);
}

#[test]
fn test_mini_vault_stem_and_unicode_heading_and_frontmatter() {
    let doc_gunluk = parse_document(
        include_str!("fixtures/vault/Gunluk/2026-08-27.md"),
        Path::new("Gunluk/2026-08-27.md"),
    );
    let doc_main = parse_document(include_str!("fixtures/vault/main.md"), Path::new("main.md"));
    let doc_fm = parse_document(
        include_str!("fixtures/vault/frontmatter_test.md"),
        Path::new("frontmatter_test.md"),
    );

    // 1. Frontmatter exclusion test:
    // doc_fm frontmatter contains `title: "[[gizli-link]] ve #gizli-tag"`
    // Body links and tags should NOT include gizli-link or #gizli-tag
    assert!(!doc_fm.links.iter().any(|l| l.target_doc == "gizli-link"));
    assert!(!doc_fm.tags.iter().any(|t| t.name.contains("gizli-tag")));

    let index = Index::build(vec![doc_gunluk.clone(), doc_main.clone(), doc_fm.clone()]);

    assert_eq!(index.doc_count(), 3);

    // 2. Stem resolution test:
    // "2026-08-27" stem resolves to "Gunluk/2026-08-27.md"
    let resolved_gunluk = index.resolve_link("2026-08-27");
    assert!(resolved_gunluk.is_some());
    assert_eq!(resolved_gunluk.unwrap().as_str(), "Gunluk/2026-08-27.md");

    // 3. Heading matching test with Turkish Unicode characters:
    let target_doc = index.get_doc(resolved_gunluk.unwrap()).unwrap();
    let heading = target_doc
        .headings
        .iter()
        .find(|h| h.matches("Günün Özeti"))
        .expect("Should match 'Günün Özeti'");
    assert_eq!(heading.slug, "günün-özeti");

    // Matches lowercase and uppercase input as well
    assert!(heading.matches("günün özeti"));
    assert!(heading.matches("GÜNÜN ÖZETİ"));
    assert!(heading.matches("günün-özeti"));

    // 4. Backlinks from main.md to Gunluk/2026-08-27.md and frontmatter_test.md
    let gunluk_id = satz_core::DocId::new("Gunluk/2026-08-27.md");
    let main_id = satz_core::DocId::new("main.md");
    let fm_id = satz_core::DocId::new("frontmatter_test.md");

    let gunluk_backlinks: Vec<_> = index.backlinks_of(&gunluk_id).collect();
    assert!(gunluk_backlinks.contains(&&main_id));

    let fm_backlinks: Vec<_> = index.backlinks_of(&fm_id).collect();
    assert!(fm_backlinks.contains(&&main_id));

    // Broken links in this mini vault should be 0
    assert_eq!(index.broken_link_count(), 0);
}

#[test]
fn test_alias_self_conflict() {
    // Tests that having an alias exactly the same as the title does not trigger
    // unnecessary warnings, and works fine.
    let doc_src = "---\ntitle: Foo\naliases: [Foo]\n---\n# Foo";
    let doc = parse_document(doc_src, Path::new("foo.md"));

    let index = Index::build(vec![doc]);

    // We can resolve it by alias or title (both "Foo")
    assert_eq!(index.resolve_link("Foo").unwrap().as_str(), "foo.md");
    // Path resolution still works
    assert_eq!(index.resolve_link("foo.md").unwrap().as_str(), "foo.md");
}

#[test]
fn test_turkish_title_key() {
    let doc = parse_document(
        "---\ntitle: İstemciler\n---\n# İstemciler",
        Path::new("i.md"),
    );
    let index = Index::build(vec![doc]);
    assert!(index.resolve_link("istemciler").is_some());
    assert!(index.resolve_link("İSTEMCİLER").is_some());
}

#[test]
fn test_relative_daily_note_resolution() {
    let now = chrono::Local::now().date_naive();
    let today_str = now.format("%Y-%m-%d").to_string();
    let yesterday_str = (now - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let tomorrow_str = (now + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let doc_today = parse_document("# Today", Path::new(&format!("daily/{}.md", today_str)));
    let doc_yesterday = parse_document(
        "# Yesterday",
        Path::new(&format!("daily/{}.md", yesterday_str)),
    );
    let doc_tomorrow = parse_document(
        "# Tomorrow",
        Path::new(&format!("daily/{}.md", tomorrow_str)),
    );

    let index = Index::build(vec![doc_today, doc_yesterday, doc_tomorrow]);
    let config = satz_core::DailyNoteConfig::default();

    assert_eq!(
        index
            .resolve_relative_daily("bugün", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", today_str)
    );
    assert_eq!(
        index
            .resolve_relative_daily("bugun", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", today_str)
    );
    assert_eq!(
        index
            .resolve_relative_daily("today", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", today_str)
    );
    assert_eq!(
        index
            .resolve_relative_daily("dün", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", yesterday_str)
    );
    assert_eq!(
        index
            .resolve_relative_daily("yesterday", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", yesterday_str)
    );
    assert_eq!(
        index
            .resolve_relative_daily("yarın", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", tomorrow_str)
    );
    assert_eq!(
        index
            .resolve_relative_daily("tomorrow", &config)
            .unwrap()
            .as_str(),
        format!("daily/{}.md", tomorrow_str)
    );
}
