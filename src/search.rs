use crate::model::Entry;

/// First index at or after `from` (wrapping) for which `matches` holds, walking forward or backward.
/// With `include_from` the entry at `from` can match; otherwise the walk starts one past it
/// and can wrap all the way back to `from`.
pub fn find_by(
    len: usize,
    from: usize,
    forward: bool,
    include_from: bool,
    matches: impl Fn(usize) -> bool,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let from = from.min(len - 1);
    let steps = if include_from { 0..len } else { 1..len + 1 };
    steps
        .map(|k| {
            if forward {
                (from + k) % len
            } else {
                (from + len - k % len) % len
            }
        })
        .find(|&i| matches(i))
}

/// Index of the next entry whose name contains `query`, wrapping around the list.
/// Like vim's smartcase, the match ignores case unless `query` has an uppercase letter.
pub fn find(
    entries: &[Entry],
    query: &str,
    from: usize,
    forward: bool,
    include_from: bool,
) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let ignore_case = !query.chars().any(char::is_uppercase);
    let needle = if ignore_case {
        query.to_lowercase()
    } else {
        query.to_string()
    };
    find_by(entries.len(), from, forward, include_from, |i| {
        let name = entries[i].display_name();
        if ignore_case {
            name.to_lowercase().contains(&needle)
        } else {
            name.contains(&needle)
        }
    })
}

/// Next entry whose name starts with `initial`, with the same smartcase rule as [`find`].
pub fn find_initial(entries: &[Entry], initial: char, from: usize, forward: bool) -> Option<usize> {
    let ignore_case = !initial.is_uppercase();
    find_by(entries.len(), from, forward, false, |i| {
        let first = entries[i].display_name().chars().next();
        first.is_some_and(|c| {
            if ignore_case {
                c.to_lowercase().eq(initial.to_lowercase())
            } else {
                c == initial
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn entries(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|n| Entry {
                name: (*n).into(),
                kind: Kind::File,
                size: 0,
                mtime: None,
            })
            .collect()
    }

    #[test]
    fn finds_substrings_forward_and_wraps() {
        let e = entries(&["src", "Cargo.toml", "README.md", "tests", "target"]);
        assert_eq!(find(&e, "t", 0, true, false), Some(1));
        assert_eq!(find(&e, "t", 4, true, false), Some(1), "wraps past the end");
        assert_eq!(find(&e, "tar", 0, true, false), Some(4));
    }

    #[test]
    fn backward_search_wraps_the_other_way() {
        let e = entries(&["src", "Cargo.toml", "README.md", "tests", "target"]);
        assert_eq!(find(&e, "t", 4, false, false), Some(3));
        assert_eq!(
            find(&e, "src", 0, false, false),
            Some(0),
            "the only match is the start itself"
        );
        assert_eq!(find(&e, "c", 0, false, false), Some(1));
    }

    #[test]
    fn include_from_lets_the_current_entry_match() {
        let e = entries(&["alpha", "beta", "alpine"]);
        assert_eq!(find(&e, "al", 0, true, true), Some(0));
        assert_eq!(find(&e, "al", 0, true, false), Some(2));
    }

    #[test]
    fn smartcase_ignores_case_until_the_query_has_a_capital() {
        let e = entries(&["readme", "README.md", "Readme.txt"]);
        assert_eq!(find(&e, "readme", 2, true, false), Some(0));
        assert_eq!(find(&e, "README", 0, true, false), Some(1));
        assert_eq!(find(&e, "Readme", 0, true, false), Some(2));
    }

    #[test]
    fn no_match_empty_query_and_empty_list_give_none() {
        let e = entries(&["a", "b"]);
        assert_eq!(find(&e, "zzz", 0, true, true), None);
        assert_eq!(find(&e, "", 0, true, true), None);
        assert_eq!(find(&[], "a", 0, true, true), None);
    }

    #[test]
    fn find_initial_matches_the_first_letter_only() {
        let e = entries(&["apple", "banana", "Berry", "cherry", "avocado"]);
        assert_eq!(find_initial(&e, 'b', 0, true), Some(1));
        assert_eq!(
            find_initial(&e, 'b', 1, true),
            Some(2),
            "smartcase lowercase matches Berry"
        );
        assert_eq!(
            find_initial(&e, 'B', 0, true),
            Some(2),
            "uppercase is exact"
        );
        assert_eq!(find_initial(&e, 'a', 4, true), Some(0), "wraps");
        assert_eq!(find_initial(&e, 'a', 0, false), Some(4), "backward wraps");
        assert_eq!(find_initial(&e, 'z', 0, true), None);
    }
}
