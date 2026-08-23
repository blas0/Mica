//! Find in scrollback.
//!
//! A pure function over text, which is why it needs no GPU to test. The
//! *highlighting* is a render concern — matches become quads, never mutated
//! cells — but deciding what matches is not.
//!
//! There used to be a fuzzy matcher here too, ranking command-palette entries.
//! The palette is gone; so is it.
//!
//! ## Literal, not regex
//!
//! Search is literal and case-insensitive-when-the-query-is-lowercase (the
//! "smart case" rule every editor uses). Regex would mean a regex crate, and the
//! whole point of this dependency graph is that nothing enters it without
//! earning its place. Regex is the obvious second step and the [`Matcher`] enum
//! is where it goes.

use std::ops::Range;

/// One match, in absolute line coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Match {
    pub line: i32,
    /// Column range within the line, in characters.
    pub start: u16,
    pub end: u16,
}

impl Match {
    pub fn columns(&self) -> Range<u16> {
        self.start..self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Matcher {
    /// Literal substring, smart-case.
    #[default]
    Literal,
}

/// A running search.
#[derive(Debug, Default, Clone)]
pub struct Search {
    query: String,
    matcher: Matcher,
    matches: Vec<Match>,
    /// Index into `matches`, for next/previous.
    current: usize,
}

impl Search {
    pub fn new() -> Search {
        Search::default()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// 1-based position of the current match, for the "3 of 17" readout.
    pub fn position(&self) -> Option<usize> {
        (!self.matches.is_empty()).then(|| self.current + 1)
    }

    pub fn current(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    /// Re-runs the search over `lines`, which yields `(line_index, text)`.
    ///
    /// The caller supplies the iterator so this function never has to know
    /// about a terminal — and so a test can search a fixed corpus.
    pub fn run<'a, I>(&mut self, query: &str, lines: I)
    where
        I: IntoIterator<Item = (i32, &'a str)>,
    {
        self.query = query.to_owned();
        self.matches.clear();
        self.current = 0;

        if query.is_empty() {
            return;
        }
        for (line, text) in lines {
            find_in_line(query, text, self.matcher, line, &mut self.matches);
        }
        self.matches.sort();
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = 0;
    }

    /// Advances to the next match, wrapping. Wrapping rather than stopping is
    /// what makes `⌘G` usable without watching a counter.
    pub fn next(&mut self) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        self.current()
    }

    pub fn previous(&mut self) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
        self.current()
    }

    /// Selects the match nearest to `line`, so opening find while looking at
    /// the middle of the scrollback does not jump to the top.
    pub fn focus_near(&mut self, line: i32) {
        if self.matches.is_empty() {
            return;
        }
        self.current = self
            .matches
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| (m.line - line).abs())
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
}

/// Smart case: a lowercase query matches case-insensitively, a query with any
/// uppercase in it matches exactly. Nobody has to reach for a toggle.
fn smart_case(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

fn find_in_line(
    query: &str,
    text: &str,
    matcher: Matcher,
    line: i32,
    out: &mut Vec<Match>,
) {
    match matcher {
        Matcher::Literal => {
            let sensitive = smart_case(query);
            // Column indices are in characters, not bytes, because the grid is
            // addressed in cells. A byte offset would put the highlight in the
            // wrong place on any line containing non-ASCII.
            let haystack: Vec<char> = if sensitive {
                text.chars().collect()
            } else {
                text.chars().flat_map(char::to_lowercase).collect()
            };
            let needle: Vec<char> = if sensitive {
                query.chars().collect()
            } else {
                query.chars().flat_map(char::to_lowercase).collect()
            };
            if needle.is_empty() || needle.len() > haystack.len() {
                return;
            }
            let mut i = 0;
            while i + needle.len() <= haystack.len() {
                if haystack[i..i + needle.len()] == needle[..] {
                    out.push(Match {
                        line,
                        start: i as u16,
                        end: (i + needle.len()) as u16,
                    });
                    // Non-overlapping: `aa` in `aaa` is one match, not two.
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORPUS: &[(i32, &str)] = &[
        (-2, "error: could not compile mica-core"),
        (-1, "warning: unused import"),
        (0, "$ cargo build"),
        (1, "   Compiling mica-gpu v0.1.0"),
        (2, "error[E0308]: mismatched types"),
    ];

    fn search(query: &str) -> Search {
        let mut s = Search::new();
        s.run(query, CORPUS.iter().map(|(l, t)| (*l, *t)));
        s
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        let s = search("");
        assert!(s.is_empty());
        assert_eq!(s.position(), None);
    }

    #[test]
    fn a_literal_query_finds_every_occurrence_in_line_order() {
        let s = search("error");
        assert_eq!(s.len(), 2);
        assert_eq!(s.matches()[0].line, -2);
        assert_eq!(s.matches()[1].line, 2);
        assert_eq!(s.matches()[0].columns(), 0..5);
    }

    #[test]
    fn a_lowercase_query_is_case_insensitive() {
        assert_eq!(search("compiling").len(), 1);
        assert_eq!(search("COMPILING").len(), 0, "an uppercase query must be exact");
    }

    #[test]
    fn a_query_with_uppercase_becomes_case_sensitive() {
        // Smart case: nobody should have to find a toggle.
        assert_eq!(search("Compiling").len(), 1);
        assert_eq!(search("mica").len(), 2, "lowercase still matches both");
    }

    #[test]
    fn columns_are_counted_in_characters_not_bytes() {
        // A byte offset would put the highlight in the wrong cell on any line
        // with non-ASCII in it.
        let mut s = Search::new();
        s.run("x", [(0, "→→x")]);
        assert_eq!(s.matches()[0].start, 2, "the arrows are three bytes each");
    }

    #[test]
    fn matches_do_not_overlap() {
        let mut s = Search::new();
        s.run("aa", [(0, "aaaa")]);
        assert_eq!(s.len(), 2);
        assert_eq!(s.matches()[0].columns(), 0..2);
        assert_eq!(s.matches()[1].columns(), 2..4);
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut s = search("error");
        assert_eq!(s.position(), Some(1));
        assert_eq!(s.next().unwrap().line, 2);
        assert_eq!(s.position(), Some(2));
        // Wrapping is what makes repeated ⌘G usable without watching a counter.
        assert_eq!(s.next().unwrap().line, -2);
        assert_eq!(s.previous().unwrap().line, 2);
    }

    #[test]
    fn navigating_an_empty_result_set_does_nothing() {
        let mut s = search("no-such-text");
        assert_eq!(s.next(), None);
        assert_eq!(s.previous(), None);
    }

    #[test]
    fn focusing_picks_the_match_nearest_the_viewport() {
        // Opening find while reading the middle of a log should not jump to
        // the top of the scrollback.
        let mut s = search("error");
        s.focus_near(2);
        assert_eq!(s.current().unwrap().line, 2);
        s.focus_near(-2);
        assert_eq!(s.current().unwrap().line, -2);
    }

    #[test]
    fn clearing_resets_everything() {
        let mut s = search("error");
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.query(), "");
        assert_eq!(s.position(), None);
    }
}
