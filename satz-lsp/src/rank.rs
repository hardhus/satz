use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Fuzzy matcher utility for scoring target strings against a query pattern.
pub struct Ranker {
    matcher: Matcher,
    /// One plain-text fuzzy pattern per whitespace-separated word; every one must match.
    patterns: Vec<Pattern>,
    buf: Vec<char>,
}

impl Ranker {
    pub fn new(query: &str) -> Self {
        // `Pattern::parse` would read `^`, `$`, `!` and a leading `'` as nucleo's own query syntax;
        // what the user types is plain text, so each word is matched literally (fuzzily).
        let patterns = query
            .split_whitespace()
            .map(|word| {
                Pattern::new(
                    word,
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
        let haystack = Utf32Str::new(target, &mut self.buf);
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
}
