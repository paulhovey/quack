use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

struct Candidate<'a>(usize, &'a str);

impl AsRef<str> for Candidate<'_> {
    fn as_ref(&self) -> &str {
        self.1
    }
}

/// Indices of `items` matching `query`, best first. An empty query keeps the original order.
pub(crate) fn filter<'a>(query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let items: Vec<Candidate<'a>> = items
        .into_iter()
        .enumerate()
        .map(|(i, s)| Candidate(i, s))
        .collect();
    if query.trim().is_empty() {
        return items.iter().map(|c| c.0).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart)
        .match_list(items, &mut matcher)
        .into_iter()
        .map(|(c, _)| c.0)
        .collect()
}

/// Char positions in `text` that `query` matched, for highlighting. Empty when it doesn't.
pub(crate) fn positions(query: &str, text: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut hits = Vec::new();
    Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart).indices(
        Utf32Str::new(text, &mut buf),
        &mut matcher,
        &mut hits,
    );
    hits.sort_unstable();
    hits.dedup();
    hits.into_iter().map(|i| i as usize).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn filters_and_ranks() {
        let items = ["models", "mcp", "memory", "help"];
        assert_eq!(super::filter("", items), [0, 1, 2, 3]);
        let hits = super::filter("mem", items);
        assert_eq!(hits.first(), Some(&2));
        assert!(!hits.contains(&3));
        assert_eq!(super::positions("mem", "memory"), [0, 1, 2]);
        assert_eq!(super::positions("rw", "rewind"), [0, 2]);
        assert!(super::positions("", "memory").is_empty());
    }
}
