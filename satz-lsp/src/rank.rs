use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Fuzzy matcher utility for scoring target strings against a query pattern.
pub struct Ranker {
    matcher: Matcher,
}

impl Default for Ranker {
    fn default() -> Self {
        Self::new()
    }
}

impl Ranker {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Returns a score if `target` matches `pattern`. Higher score means better match.
    pub fn score(&mut self, pattern_str: &str, target: &str) -> Option<u32> {
        if pattern_str.is_empty() {
            return Some(0);
        }

        let pattern = Pattern::parse(pattern_str, CaseMatching::Smart, Normalization::Smart);

        let mut buf = Vec::new();
        let utf32_target = Utf32Str::new(target, &mut buf);

        pattern.score(utf32_target, &mut self.matcher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ranker_fuzzy_match() {
        let mut ranker = Ranker::new();
        let score1 = ranker.score("sat", "satz-project");
        let score2 = ranker.score("xyz", "satz-project");

        assert!(score1.is_some());
        assert!(score2.is_none());
        assert!(score1.unwrap() > 0);
    }
}
