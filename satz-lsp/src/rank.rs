use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Fuzzy matcher utility for scoring target strings against a query pattern.
pub struct Ranker {
    matcher: Matcher,
    pattern: Pattern,
    buf: Vec<char>,
    is_empty_query: bool,
}

impl Ranker {
    pub fn new(query: &str) -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            pattern: Pattern::parse(query, CaseMatching::Smart, Normalization::Smart),
            buf: Vec::new(),
            is_empty_query: query.is_empty(),
        }
    }

    /// Returns a score if `target` matches `pattern`. Higher score means better match.
    pub fn score(&mut self, target: &str) -> Option<u32> {
        if self.is_empty_query {
            return Some(0);
        }
        self.buf.clear();
        self.pattern
            .score(Utf32Str::new(target, &mut self.buf), &mut self.matcher)
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
}
