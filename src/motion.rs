//! Where a motion would put the cursor, without moving it. Both plain movement and
//! operators such as `d3j` resolve their target here.

use crate::{keys::Command, model::Entry, search};

pub struct Ctx<'a> {
    pub entries: &'a [Entry],
    pub cursor: usize,
    pub viewport: u16,
    pub last_search: Option<&'a str>,
    /// Letter and direction of the last `f` jump.
    pub last_find: Option<(char, bool)>,
}

/// Index the cursor lands on, or the message to show when the motion finds nothing.
pub fn target(
    command: Command,
    count: Option<usize>,
    arg: Option<char>,
    ctx: &Ctx,
) -> Result<usize, String> {
    let Some(last) = ctx.entries.len().checked_sub(1) else {
        return Ok(0);
    };
    let n = count.unwrap_or(1);
    let step = isize::try_from(n).unwrap_or(isize::MAX);
    let half = isize::try_from(ctx.viewport / 2).unwrap_or(1).max(1);
    let page = isize::try_from(ctx.viewport)
        .unwrap_or(1)
        .saturating_sub(2)
        .max(1);
    let shift = |delta: isize| ctx.cursor.saturating_add_signed(delta).min(last);
    match command {
        Command::Down => Ok(shift(step)),
        Command::Up => Ok(shift(-step)),
        Command::HalfPageDown => Ok(shift(half.saturating_mul(step))),
        Command::HalfPageUp => Ok(shift(-half.saturating_mul(step))),
        Command::PageDown => Ok(shift(page.saturating_mul(step))),
        Command::PageUp => Ok(shift(-page.saturating_mul(step))),
        Command::First => Ok(count.map_or(0, |c| c.saturating_sub(1)).min(last)),
        Command::Last => Ok(count.map_or(last, |c| c.saturating_sub(1)).min(last)),
        Command::SearchNext | Command::SearchPrev => {
            let query = ctx.last_search.ok_or("no previous search")?;
            let forward = command == Command::SearchNext;
            repeat(ctx.cursor, n, |at| {
                search::find(ctx.entries, query, at, forward, false)
            })
            .ok_or_else(|| format!("pattern not found: {query}"))
        }
        Command::Find | Command::FindBack => {
            let letter = arg.ok_or("no letter given")?;
            by_initial(ctx, letter, command == Command::Find, n)
        }
        Command::FindRepeat | Command::FindRepeatBack => {
            let (letter, forward) = ctx.last_find.ok_or("no previous letter jump")?;
            by_initial(ctx, letter, forward == (command == Command::FindRepeat), n)
        }
        _ => Err("not a motion".into()),
    }
}

fn by_initial(ctx: &Ctx, letter: char, forward: bool, times: usize) -> Result<usize, String> {
    repeat(ctx.cursor, times, |at| {
        search::find_initial(ctx.entries, letter, at, forward)
    })
    .ok_or_else(|| format!("no name starts with {letter}"))
}

/// Applies `step` up to `times` times. Stops with `None` as soon as one step finds nothing.
fn repeat(from: usize, times: usize, step: impl Fn(usize) -> Option<usize>) -> Option<usize> {
    let mut at = from;
    for _ in 0..times.min(1000) {
        at = step(at)?;
    }
    Some(at)
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

    fn at(
        cmd: Command,
        count: Option<usize>,
        arg: Option<char>,
        cursor: usize,
    ) -> Result<usize, String> {
        let e = entries(&["apple", "banana", "berry", "cherry", "avocado", "blueberry"]);
        let ctx = Ctx {
            entries: &e,
            cursor,
            viewport: 6,
            last_search: Some("berry"),
            last_find: Some(('b', true)),
        };
        target(cmd, count, arg, &ctx)
    }

    #[test]
    fn line_motions_clamp_to_the_list() {
        assert_eq!(at(Command::Down, None, None, 0), Ok(1));
        assert_eq!(at(Command::Down, Some(3), None, 4), Ok(5));
        assert_eq!(at(Command::Up, Some(9), None, 2), Ok(0));
        assert_eq!(at(Command::First, None, None, 4), Ok(0));
        assert_eq!(at(Command::Last, None, None, 0), Ok(5));
        assert_eq!(at(Command::First, Some(3), None, 0), Ok(2));
        assert_eq!(at(Command::Last, Some(99), None, 0), Ok(5));
    }

    #[test]
    fn page_motions_use_the_viewport_and_never_stand_still() {
        assert_eq!(at(Command::HalfPageDown, None, None, 0), Ok(3));
        assert_eq!(at(Command::PageDown, None, None, 0), Ok(4));
        assert_eq!(at(Command::HalfPageUp, Some(2), None, 5), Ok(0));
    }

    #[test]
    fn search_motions_use_the_last_search_and_wrap() {
        assert_eq!(at(Command::SearchNext, None, None, 0), Ok(2));
        assert_eq!(at(Command::SearchNext, Some(2), None, 0), Ok(5));
        assert_eq!(at(Command::SearchNext, Some(3), None, 0), Ok(2), "wraps");
        assert_eq!(at(Command::SearchPrev, None, None, 5), Ok(2));
    }

    #[test]
    fn letter_jumps_count_and_repeat() {
        assert_eq!(at(Command::Find, None, Some('b'), 0), Ok(1));
        assert_eq!(at(Command::Find, Some(2), Some('b'), 0), Ok(2));
        assert_eq!(at(Command::FindBack, None, Some('a'), 4), Ok(0));
        assert_eq!(at(Command::FindRepeat, None, None, 1), Ok(2));
        assert_eq!(at(Command::FindRepeatBack, None, None, 2), Ok(1));
    }

    #[test]
    fn misses_come_back_as_messages() {
        assert_eq!(
            at(Command::Find, None, Some('z'), 0),
            Err("no name starts with z".into())
        );
        assert_eq!(
            at(Command::Find, Some(4), Some('b'), 0),
            Ok(1),
            "a count past the last match wraps around instead of failing"
        );
    }

    #[test]
    fn an_empty_list_stays_at_zero_and_non_motions_are_rejected() {
        let ctx = Ctx {
            entries: &[],
            cursor: 0,
            viewport: 10,
            last_search: None,
            last_find: None,
        };
        assert_eq!(target(Command::Down, Some(5), None, &ctx), Ok(0));
        assert_eq!(
            at(Command::Enter, None, None, 0),
            Err("not a motion".into())
        );
    }

    #[test]
    fn missing_history_is_reported() {
        let e = entries(&["a"]);
        let ctx = Ctx {
            entries: &e,
            cursor: 0,
            viewport: 10,
            last_search: None,
            last_find: None,
        };
        assert_eq!(
            target(Command::SearchNext, None, None, &ctx),
            Err("no previous search".into())
        );
        assert_eq!(
            target(Command::FindRepeat, None, None, &ctx),
            Err("no previous letter jump".into())
        );
    }
}
