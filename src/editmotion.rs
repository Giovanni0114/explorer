//! Cursor motions inside the text editor, as pure functions of the buffer. An operator such as
//! `d` turns the same motion into a range to act on.

use unicode_segmentation::UnicodeSegmentation;

use crate::textbuf::{Buffer, Pos, byte_of_col, grapheme_len};

/// How a motion's end relates to the text it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Up to but not including the target.
    Exclusive,
    /// Including the character at the target.
    Inclusive,
    /// Whole lines from the cursor line to the target line.
    Linewise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub pos: Pos,
    pub kind: Kind,
}

/// A range for an operator to act on. `end` is exclusive unless the range is linewise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Pos,
    pub end: Pos,
    pub linewise: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Find {
    pub ch: char,
    pub forward: bool,
    pub till: bool,
}

fn class_at(buf: &Buffer, pos: Pos, big: bool) -> u8 {
    let line = buf.line(pos.line);
    let start = byte_of_col(line, pos.col);
    let Some(c) = line[start..].chars().next() else {
        return 0;
    };
    match () {
        _ if c.is_whitespace() => 0,
        _ if big => 1,
        _ if c.is_alphanumeric() || c == '_' => 1,
        _ => 2,
    }
}

pub fn first_non_blank(buf: &Buffer, line: usize) -> usize {
    let text = buf.line(line);
    text.graphemes(true)
        .position(|g| !g.chars().all(char::is_whitespace))
        .unwrap_or_else(|| grapheme_len(text).saturating_sub(1))
}

/// Start of the next word. An empty line counts as a word.
pub fn word_forward(buf: &Buffer, from: Pos, big: bool) -> Pos {
    let mut p = from;
    let here = class_at(buf, p, big);
    if here != 0 {
        while p.col < buf.line_len(p.line) && class_at(buf, p, big) == here {
            p.col += 1;
        }
    }
    loop {
        if p.col >= buf.line_len(p.line) {
            if p.line + 1 >= buf.line_count() {
                return Pos::new(p.line, buf.line_len(p.line));
            }
            p = Pos::new(p.line + 1, 0);
            if buf.line_len(p.line) == 0 {
                return p;
            }
            continue;
        }
        if class_at(buf, p, big) != 0 {
            return p;
        }
        p.col += 1;
    }
}

pub fn word_back(buf: &Buffer, from: Pos, big: bool) -> Pos {
    let mut p = from;
    loop {
        if p.col == 0 {
            if p.line == 0 {
                return Pos::new(0, 0);
            }
            p = Pos::new(p.line - 1, buf.line_len(p.line - 1));
            if p.col == 0 {
                return p;
            }
            continue;
        }
        p.col -= 1;
        if p.col < buf.line_len(p.line) && class_at(buf, p, big) != 0 {
            break;
        }
    }
    let class = class_at(buf, p, big);
    while p.col > 0 && class_at(buf, Pos::new(p.line, p.col - 1), big) == class {
        p.col -= 1;
    }
    p
}

/// Last character of the current or next word.
pub fn word_end(buf: &Buffer, from: Pos, big: bool) -> Pos {
    let mut p = Pos::new(from.line, from.col + 1);
    loop {
        if p.col >= buf.line_len(p.line) {
            if p.line + 1 >= buf.line_count() {
                return Pos::new(p.line, buf.line_len(p.line).saturating_sub(1));
            }
            p = Pos::new(p.line + 1, 0);
            continue;
        }
        if class_at(buf, p, big) != 0 {
            break;
        }
        p.col += 1;
    }
    let class = class_at(buf, p, big);
    while p.col + 1 < buf.line_len(p.line)
        && class_at(buf, Pos::new(p.line, p.col + 1), big) == class
    {
        p.col += 1;
    }
    p
}

/// The last character of the word under the cursor. `cw` uses it so that changing a word keeps the space after it.
pub fn end_of_current_word(buf: &Buffer, from: Pos, big: bool) -> Pos {
    let class = class_at(buf, from, big);
    let mut p = from;
    while p.col + 1 < buf.line_len(p.line)
        && class_at(buf, Pos::new(p.line, p.col + 1), big) == class
    {
        p.col += 1;
    }
    p
}

pub fn find_char(
    buf: &Buffer,
    from: Pos,
    find: Find,
    count: usize,
    is_repeat: bool,
) -> Option<Pos> {
    let graphemes: Vec<&str> = buf.line(from.line).graphemes(true).collect();
    let mut wanted = [0u8; 4];
    let wanted = find.ch.encode_utf8(&mut wanted) as &str;
    let mut col = from.col;
    // Repeating `t` from right before the character must not stay where it is.
    let mut skip_adjacent = find.till && is_repeat;
    for _ in 0..count {
        let found = if find.forward {
            let start = col + 1 + usize::from(skip_adjacent);
            (start..graphemes.len()).find(|&i| graphemes[i] == wanted)?
        } else {
            let end = col.saturating_sub(usize::from(skip_adjacent));
            (0..end).rev().find(|&i| graphemes[i] == wanted)?
        };
        skip_adjacent = false;
        col = found;
    }
    Some(Pos::new(
        from.line,
        match (find.till, find.forward) {
            (true, true) => col - 1,
            (true, false) => col + 1,
            (false, _) => col,
        },
    ))
}

pub fn paragraph_forward(buf: &Buffer, from: Pos) -> Pos {
    let last = buf.line_count() - 1;
    let mut line = from.line;
    while line < last && buf.line_len(line) == 0 {
        line += 1;
    }
    while line < last && buf.line_len(line) != 0 {
        line += 1;
    }
    if buf.line_len(line) == 0 {
        Pos::new(line, 0)
    } else {
        Pos::new(last, buf.line_len(last))
    }
}

pub fn paragraph_back(buf: &Buffer, from: Pos) -> Pos {
    let mut line = from.line;
    while line > 0 && buf.line_len(line) == 0 {
        line -= 1;
    }
    while line > 0 && buf.line_len(line) != 0 {
        line -= 1;
    }
    Pos::new(line, 0)
}

/// Finds `query` as a substring, wrapping around the buffer. Ignores case unless the query has a capital.
pub fn search(buf: &Buffer, query: &str, from: Pos, forward: bool) -> Option<Pos> {
    if query.is_empty() {
        return None;
    }
    let ignore_case = !query.chars().any(char::is_uppercase);
    let needle = if ignore_case {
        query.to_lowercase()
    } else {
        query.to_string()
    };
    let count = buf.line_count();
    let hits = |line: usize| -> Vec<usize> {
        let text = buf.line(line);
        let hay = if ignore_case {
            text.to_lowercase()
        } else {
            text.to_string()
        };
        // Lowercasing can change byte lengths, so only trust the offsets when it did not.
        let exact = hay.len() == text.len();
        hay.match_indices(&needle)
            .filter(|_| exact)
            .map(|(i, _)| grapheme_len(&text[..i.min(text.len())]))
            .collect()
    };
    for step in 0..=count {
        let line = if forward {
            (from.line + step) % count
        } else {
            (from.line + count - step % count) % count
        };
        let cols = hits(line);
        let found = match (step, forward) {
            (0, true) => cols.into_iter().find(|&c| c > from.col),
            (0, false) => cols.into_iter().rev().find(|&c| c < from.col),
            (s, true) if s == count => cols.into_iter().find(|&c| c <= from.col),
            (s, false) if s == count => cols.into_iter().rev().find(|&c| c >= from.col),
            (_, true) => cols.into_iter().next(),
            (_, false) => cols.into_iter().next_back(),
        };
        if let Some(col) = found {
            return Some(Pos::new(line, col));
        }
    }
    None
}

/// Where an operator's range ends for a motion. `w` stops at the end of the last word on the line
/// instead of running into the next line, as vim's `dw` does.
pub fn range_for(buf: &Buffer, cursor: Pos, target: Target, word_motion: bool) -> Range {
    let mut target = target;
    if word_motion && target.kind == Kind::Exclusive && target.pos.line > cursor.line {
        let line = target.pos.line - 1;
        target.pos = Pos::new(line, buf.line_len(line));
    }
    let (a, b) = if cursor <= target.pos {
        (cursor, target.pos)
    } else {
        (target.pos, cursor)
    };
    let whole_lines = |a: usize, b: usize| Range {
        start: Pos::new(a, 0),
        end: Pos::new(b, buf.line_len(b)),
        linewise: true,
    };
    match target.kind {
        Kind::Linewise => whole_lines(a.line, b.line),
        Kind::Inclusive => Range {
            start: a,
            end: Pos::new(b.line, (b.col + 1).min(buf.line_len(b.line))),
            linewise: false,
        },
        Kind::Exclusive if b.col == 0 && b.line > a.line && !word_motion => {
            let end_line = b.line - 1;
            if a.col <= first_non_blank(buf, a.line) {
                whole_lines(a.line, end_line)
            } else {
                Range {
                    start: a,
                    end: Pos::new(end_line, buf.line_len(end_line)),
                    linewise: false,
                }
            }
        }
        Kind::Exclusive => Range {
            start: a,
            end: b,
            linewise: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::from_bytes(text.as_bytes()).unwrap()
    }

    fn p(line: usize, col: usize) -> Pos {
        Pos::new(line, col)
    }

    #[test]
    fn w_moves_between_word_starts_and_treats_punctuation_as_words() {
        let b = buf("foo.bar baz\n  qux\n\nend\n");
        assert_eq!(word_forward(&b, p(0, 0), false), p(0, 3), "foo then .");
        assert_eq!(word_forward(&b, p(0, 3), false), p(0, 4));
        assert_eq!(word_forward(&b, p(0, 4), false), p(0, 8));
        assert_eq!(
            word_forward(&b, p(0, 8), false),
            p(1, 2),
            "skips the indent on the next line"
        );
        assert_eq!(
            word_forward(&b, p(1, 2), false),
            p(2, 0),
            "an empty line is a word"
        );
        assert_eq!(word_forward(&b, p(2, 0), false), p(3, 0));
        assert_eq!(
            word_forward(&b, p(3, 0), false),
            p(3, 3),
            "the end of the buffer"
        );
    }

    #[test]
    fn capital_w_only_stops_at_blanks() {
        let b = buf("foo.bar baz\n");
        assert_eq!(word_forward(&b, p(0, 0), true), p(0, 8));
        assert_eq!(word_back(&b, p(0, 10), true), p(0, 8));
        assert_eq!(word_end(&b, p(0, 0), true), p(0, 6));
    }

    #[test]
    fn b_moves_to_word_starts_backwards() {
        let b = buf("foo.bar baz\n\n  qux\n");
        assert_eq!(word_back(&b, p(0, 10), false), p(0, 8));
        assert_eq!(word_back(&b, p(0, 8), false), p(0, 4));
        assert_eq!(word_back(&b, p(0, 4), false), p(0, 3));
        assert_eq!(word_back(&b, p(0, 3), false), p(0, 0));
        assert_eq!(word_back(&b, p(0, 0), false), p(0, 0));
        assert_eq!(
            word_back(&b, p(2, 2), false),
            p(1, 0),
            "stops on the empty line"
        );
        assert_eq!(word_back(&b, p(1, 0), false), p(0, 8));
    }

    #[test]
    fn e_moves_to_word_ends_and_never_stays_put() {
        let b = buf("foo.bar  baz\nqux\n");
        assert_eq!(word_end(&b, p(0, 0), false), p(0, 2));
        assert_eq!(word_end(&b, p(0, 2), false), p(0, 3));
        assert_eq!(word_end(&b, p(0, 3), false), p(0, 6));
        assert_eq!(word_end(&b, p(0, 6), false), p(0, 11));
        assert_eq!(
            word_end(&b, p(0, 11), false),
            p(1, 2),
            "across the line break"
        );
        assert_eq!(word_end(&b, p(1, 2), false), p(1, 2), "nothing further");
    }

    #[test]
    fn the_end_of_the_current_word_backs_cw() {
        let b = buf("foo.bar baz\n");
        assert_eq!(end_of_current_word(&b, p(0, 0), false), p(0, 2));
        assert_eq!(
            end_of_current_word(&b, p(0, 2), false),
            p(0, 2),
            "already at the end"
        );
        assert_eq!(end_of_current_word(&b, p(0, 4), false), p(0, 6));
    }

    #[test]
    fn unicode_words_use_their_own_class() {
        let b = buf("zażółć gęślą\n");
        assert_eq!(word_forward(&b, p(0, 0), false), p(0, 7));
        assert_eq!(word_end(&b, p(0, 0), false), p(0, 5));
    }

    #[test]
    fn find_char_counts_occurrences_and_till_stops_short() {
        let b = buf("a,b,c,d\n");
        let f = |ch, forward, till| Find { ch, forward, till };
        assert_eq!(
            find_char(&b, p(0, 0), f(',', true, false), 1, false),
            Some(p(0, 1))
        );
        assert_eq!(
            find_char(&b, p(0, 0), f(',', true, false), 3, false),
            Some(p(0, 5))
        );
        assert_eq!(
            find_char(&b, p(0, 0), f(',', true, true), 1, false),
            Some(p(0, 0))
        );
        assert_eq!(
            find_char(&b, p(0, 6), f(',', false, false), 2, false),
            Some(p(0, 3))
        );
        assert_eq!(
            find_char(&b, p(0, 6), f(',', false, true), 1, false),
            Some(p(0, 6))
        );
        assert_eq!(find_char(&b, p(0, 0), f('z', true, false), 1, false), None);
        assert_eq!(
            find_char(&b, p(0, 0), f(',', true, false), 4, false),
            None,
            "not enough matches"
        );
    }

    #[test]
    fn repeating_till_moves_on_instead_of_staying() {
        let b = buf("a,b,c,d\n");
        let t = Find {
            ch: ',',
            forward: true,
            till: true,
        };
        assert_eq!(find_char(&b, p(0, 0), t, 1, true), Some(p(0, 2)));
        let back = Find {
            ch: ',',
            forward: false,
            till: true,
        };
        assert_eq!(find_char(&b, p(0, 6), back, 1, true), Some(p(0, 4)));
    }

    #[test]
    fn paragraphs_jump_between_blank_lines() {
        let b = buf("a\nb\n\nc\nd\n\n\ne\n");
        assert_eq!(paragraph_forward(&b, p(0, 0)), p(2, 0));
        assert_eq!(paragraph_forward(&b, p(2, 0)), p(5, 0));
        assert_eq!(
            paragraph_forward(&b, p(5, 0)),
            p(7, 1),
            "the end of the buffer"
        );
        assert_eq!(paragraph_back(&b, p(4, 0)), p(2, 0));
        assert_eq!(paragraph_back(&b, p(2, 0)), p(0, 0));
        assert_eq!(paragraph_back(&b, p(7, 0)), p(6, 0));
    }

    #[test]
    fn search_finds_substrings_wraps_and_uses_smartcase() {
        let b = buf("Alpha beta\nbeta gamma\nBeta\n");
        assert_eq!(search(&b, "beta", p(0, 0), true), Some(p(0, 6)));
        assert_eq!(search(&b, "beta", p(0, 6), true), Some(p(1, 0)));
        assert_eq!(
            search(&b, "beta", p(1, 0), true),
            Some(p(2, 0)),
            "case is ignored for lowercase"
        );
        assert_eq!(search(&b, "beta", p(2, 0), true), Some(p(0, 6)), "wraps");
        assert_eq!(
            search(&b, "Beta", p(0, 0), true),
            Some(p(2, 0)),
            "a capital makes it exact"
        );
        assert_eq!(search(&b, "beta", p(1, 0), false), Some(p(0, 6)));
        assert_eq!(
            search(&b, "beta", p(0, 6), false),
            Some(p(2, 0)),
            "backwards wraps"
        );
        assert_eq!(search(&b, "nope", p(0, 0), true), None);
        assert_eq!(search(&b, "", p(0, 0), true), None);
        assert_eq!(
            search(&buf("only one\n"), "one", p(0, 5), true),
            Some(p(0, 5)),
            "wraps onto itself"
        );
    }

    #[test]
    fn ranges_follow_the_motion_kind() {
        let b = buf("hello world\nsecond line\nthird\n");
        let r = |cursor, pos, kind| range_for(&b, cursor, Target { pos, kind }, false);
        assert_eq!(
            r(p(0, 2), p(0, 6), Kind::Exclusive),
            Range {
                start: p(0, 2),
                end: p(0, 6),
                linewise: false
            }
        );
        assert_eq!(
            r(p(0, 2), p(0, 6), Kind::Inclusive),
            Range {
                start: p(0, 2),
                end: p(0, 7),
                linewise: false
            }
        );
        assert_eq!(
            r(p(1, 3), p(0, 5), Kind::Linewise),
            Range {
                start: p(0, 0),
                end: p(1, 11),
                linewise: true
            }
        );
        assert_eq!(
            r(p(0, 6), p(0, 2), Kind::Exclusive),
            Range {
                start: p(0, 2),
                end: p(0, 6),
                linewise: false
            },
            "backwards motions swap the ends"
        );
    }

    #[test]
    fn an_inclusive_end_on_an_empty_line_stays_put() {
        let b = buf("\nx\n");
        let range = range_for(
            &b,
            p(0, 0),
            Target {
                pos: p(0, 0),
                kind: Kind::Inclusive,
            },
            false,
        );
        assert_eq!((range.start, range.end), (p(0, 0), p(0, 0)));
    }

    #[test]
    fn an_exclusive_motion_ending_in_column_zero_backs_off_the_line_break() {
        let b = buf("  keep this\nfoo bar\nbaz\n");
        // from the middle of a line to the start of a later one: stops at the end of the previous line
        let mid = range_for(
            &b,
            p(0, 5),
            Target {
                pos: p(2, 0),
                kind: Kind::Exclusive,
            },
            false,
        );
        assert_eq!(
            mid,
            Range {
                start: p(0, 5),
                end: p(1, 7),
                linewise: false
            }
        );
        // starting at or before the first non-blank makes it whole lines
        let whole = range_for(
            &b,
            p(0, 2),
            Target {
                pos: p(2, 0),
                kind: Kind::Exclusive,
            },
            false,
        );
        assert_eq!(
            whole,
            Range {
                start: p(0, 0),
                end: p(1, 7),
                linewise: true
            }
        );
    }

    #[test]
    fn dw_on_the_last_word_of_a_line_stops_at_its_end() {
        let b = buf("foo\nbar\n");
        let target = Target {
            pos: word_forward(&b, p(0, 0), false),
            kind: Kind::Exclusive,
        };
        assert_eq!(target.pos, p(1, 0));
        let range = range_for(&b, p(0, 0), target, true);
        assert_eq!(
            range,
            Range {
                start: p(0, 0),
                end: p(0, 3),
                linewise: false
            }
        );
    }

    #[test]
    fn first_non_blank_skips_indent_and_handles_blank_lines() {
        let b = buf("    x\n\n   \nplain\n");
        assert_eq!(first_non_blank(&b, 0), 4);
        assert_eq!(first_non_blank(&b, 1), 0);
        assert_eq!(
            first_non_blank(&b, 2),
            2,
            "a blank line ends on its last column"
        );
        assert_eq!(first_non_blank(&b, 3), 0);
    }
}
