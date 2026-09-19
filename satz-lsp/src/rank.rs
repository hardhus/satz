use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Fuzzy matcher utility for scoring target strings against a query pattern.
pub struct Ranker {
    matcher: Matcher,
    /// One plain-text fuzzy pattern per whitespace-separated word; every one must match.
    patterns: Vec<Pattern>,
    buf: Vec<char>,
}

/// Puts the Turkish I family on the same footing as ASCII `I`/`i`, for the query AND the target:
/// `İ` -> `I`, `ı` -> `i`, and the combining dot above (`U+0307`, how a decomposed `İ` is written)
/// is dropped. nucleo alone treats `İ`/`ı` as unrelated to `i`/`I`, so `işlem` would never find
/// `İşlem`. Keeping capital and small letters apart preserves nucleo's smart case: a query with a
/// capital (`İşlem`) stays case sensitive, one without (`işlem`) finds both.
fn fold_turkish_i(text: &str) -> String {
    text.chars()
        .filter(|&c| c != '\u{307}')
        .map(|c| match c {
            'İ' => 'I',
            'ı' => 'i',
            other => other,
        })
        .collect()
}

impl Ranker {
    pub fn new(query: &str) -> Self {
        // `Pattern::parse` would read `^`, `$`, `!` and a leading `'` as nucleo's own query syntax;
        // what the user types is plain text, so each word is matched literally (fuzzily).
        let patterns = query
            .split_whitespace()
            .map(|word| {
                Pattern::new(
                    &fold_turkish_i(word),
                    CaseMatching::Smart,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                )
            })
            .collect();
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            patterns,
            buf: Vec::new(),
        }
    }

    /// Returns a score if `target` matches `pattern`. Higher score means better match.
    pub fn score(&mut self, target: &str) -> Option<u32> {
        if self.patterns.is_empty() {
            return Some(0);
        }
        self.buf.clear();
        let folded = fold_turkish_i(target);
        let haystack = Utf32Str::new(&folded, &mut self.buf);
        let mut total = 0u32;
        for pattern in &self.patterns {
            total = total.saturating_add(pattern.score(haystack, &mut self.matcher)?);
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ranker_fuzzy_match() {
        let mut ranker = Ranker::new("sat");
        let score1 = ranker.score("satz-project");
        assert!(score1.is_some());
        assert!(score1.unwrap() > 0);

        let mut ranker_miss = Ranker::new("xyz");
        let score2 = ranker_miss.score("satz-project");
        assert!(score2.is_none());
    }

    fn matches(query: &str, target: &str) -> bool {
        Ranker::new(query).score(target).is_some()
    }

    #[test]
    fn special_characters_in_the_query_are_ordinary_text() {
        // nucleo's own syntax would read `^` as "starts with", `$` as "ends with", `!` as
        // "does not contain" and a leading `'` as "exact substring".
        assert!(matches("^foo", "x^foo"));
        assert!(!matches("^foo", "foo"));
        assert!(matches("foo$", "foo$bar"));
        assert!(!matches("foo$", "foo"));
        assert!(matches("!foo", "a!foo"));
        assert!(!matches("!foo", "foo"));
        assert!(matches("'foo", "it's 'foo"));
        assert!(!matches("'foo", "foo"));
    }

    #[test]
    fn several_words_must_all_match_in_any_order() {
        assert!(matches("foo bar", "bar and foo"));
        assert!(matches("foo bar", "foobar"));
        assert!(!matches("foo bar", "only foo here"));
        assert!(!matches("foo bar", "only bar here"));
    }

    #[test]
    fn empty_and_blank_queries_match_everything() {
        assert_eq!(Ranker::new("").score("anything"), Some(0));
        assert_eq!(Ranker::new("   ").score("anything"), Some(0));
        assert_eq!(Ranker::new("\t").score(""), Some(0));
    }

    #[test]
    fn case_is_smart_and_turkish_letters_work() {
        assert!(matches("foo", "FOO bar"));
        assert!(matches("Foo", "Foo bar"));
        assert!(!matches("Foo", "foo bar"));
        assert!(matches("işlem", "işlem notları"));
        assert!(matches("kağ", "Kağıt"));
        assert!(matches("ğı", "kağıt"));
    }

    #[test]
    fn a_better_match_scores_higher() {
        let mut ranker = Ranker::new("sat");
        let exact = ranker.score("sat").unwrap();
        let scattered = ranker.score("s-a-t stuff").unwrap();
        assert!(exact > scattered);
    }

    // ---- the Turkish I family: İ ı I i ----

    #[test]
    fn a_lowercase_query_finds_dotted_and_dotless_capitals() {
        assert!(matches("işlem", "İşlem notları"));
        assert!(matches("i", "İstanbul"));
        assert!(matches("istanbul", "İstanbul"));
        assert!(matches("ışık", "Işık"));
        assert!(matches("işlem", "Işlem"));
        assert!(matches("ısı", "ISI"));
    }

    #[test]
    fn an_uppercase_query_keeps_smart_case_for_the_family() {
        assert!(matches("İşlem", "İşlem notları"));
        assert!(matches("Işlem", "İşlem"));
        assert!(matches("İ", "İstanbul"));
        // Like `Foo` vs `foo`: a capital in the query makes it case sensitive.
        assert!(!matches("İşlem", "işlem notları"));
        assert!(!matches("İ", "istanbul"));
    }

    #[test]
    fn a_decomposed_capital_i_with_dot_matches_like_the_composed_one() {
        assert!(matches("işlem", "I\u{307}şlem"));
        assert!(matches("İşlem", "I\u{307}şlem"));
        assert!(matches("i\u{307}şlem", "İşlem"));
    }

    #[test]
    fn other_letters_and_words_are_unaffected() {
        assert!(matches("ğüş", "Ağüşt"));
        assert!(!matches("xyz", "İşlem"));
        assert!(matches("iş not", "Not defteri İş"));
    }
}
