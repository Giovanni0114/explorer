//! Pure geometry. The cursor row of every level sits on one shared center row,
//! so the current path reads as a single flat line.

use std::ops::Range;

/// Cells between two columns, holding the brace.
pub const GUTTER: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub level: usize,
    pub x: u16,
    pub width: u16,
}

/// Places the trailing columns that fit `viewport`, flush left.
/// Columns before `keep_from` may be dropped from the left. Later ones never are, and when even
/// they do not fit they share the space equally.
pub fn place_columns(widths: &[u16], viewport: u16, keep_from: usize) -> Vec<Placed> {
    if widths.is_empty() {
        return Vec::new();
    }
    let keep_from = keep_from.min(widths.len() - 1);
    let span = |from: usize| -> u32 {
        let n = (widths.len() - from) as u32;
        widths[from..].iter().map(|&w| u32::from(w)).sum::<u32>() + u32::from(GUTTER) * (n - 1)
    };
    let start = (0..=keep_from)
        .find(|&s| span(s) <= u32::from(viewport))
        .unwrap_or(keep_from);
    let kept = &widths[start..];
    let n = kept.len() as u16;
    let cap = (viewport.saturating_sub(GUTTER * (n - 1)) / n).max(1);
    let fits = span(start) <= u32::from(viewport);
    let mut x = 0;
    kept.iter()
        .enumerate()
        .map(|(i, &w)| {
            let width = if fits { w } else { w.min(cap) };
            let placed = Placed {
                level: start + i,
                x,
                width,
            };
            x += width + GUTTER;
            placed
        })
        .collect()
}

/// The narrowest a file preview or editor may get before the tree gives up room for it.
pub const CONTENT_MIN: u16 = 24;

/// Places the directory columns inside `tree_percent` of the width and, when there is a file
/// preview or editor, gives it everything to the right of them. Its entry is last, at index `dirs.len()`.
pub fn place(
    dirs: &[u16],
    content: bool,
    viewport: u16,
    keep_from: usize,
    tree_percent: u8,
) -> Vec<Placed> {
    let share = (u32::from(viewport) * u32::from(tree_percent.clamp(1, 100)) / 100) as u16;
    // The share limits the ancestors. The focused column and the one after it are shown whole when the screen allows.
    let keep = keep_from.min(dirs.len().saturating_sub(1));
    let needed = dirs.get(keep..).map_or(0, |kept| {
        kept.iter().sum::<u16>() + GUTTER * (kept.len() as u16).saturating_sub(1)
    });
    let limit = if content {
        viewport.saturating_sub(GUTTER + CONTENT_MIN)
    } else {
        viewport
    };
    let budget = share.max(needed).min(limit).max(1);
    let mut placed = place_columns(dirs, budget, keep_from);
    if content {
        let x = placed.last().map_or(0, |p| p.x + p.width + GUTTER);
        placed.push(Placed {
            level: dirs.len(),
            x,
            width: viewport.saturating_sub(x).max(1),
        });
    }
    placed
}

/// Screen row (relative to the tree area, may be off-screen) of entry `i`.
pub fn row_y(center: u16, cursor: usize, i: usize) -> i32 {
    i32::from(center) + i as i32 - cursor as i32
}

/// Entries whose rows fall inside a tree area of `height` rows.
pub fn visible_range(len: usize, cursor: usize, center: u16, height: u16) -> Range<usize> {
    let first = cursor.saturating_sub(usize::from(center));
    let end = (cursor + usize::from(height.saturating_sub(center))).min(len);
    first.min(end)..end
}

/// A `{` brace: its tip touches the parent's cursor row (the center row) and its
/// ends run to the first and last visible entry of the child level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brace {
    pub top: u16,
    pub bottom: u16,
    pub tip: u16,
    pub clipped_top: bool,
    pub clipped_bottom: bool,
}

pub fn brace(height: u16, center: u16, len: usize, cursor: usize) -> Brace {
    let len = len.max(1);
    let top = row_y(center, cursor, 0);
    let bottom = row_y(center, cursor, len - 1);
    let last_row = i32::from(height) - 1;
    Brace {
        top: top.clamp(0, last_row) as u16,
        bottom: bottom.clamp(0, last_row) as u16,
        tip: center,
        clipped_top: top < 0,
        clipped_bottom: bottom > last_row,
    }
}

/// Rows `(top, count)` for a block of `lines` lines that has no cursor, such as a file preview.
/// Short blocks are centered on the center row, and tall ones fill the whole height.
pub fn block_rows(height: u16, center: u16, lines: usize) -> (u16, u16) {
    let count = lines.clamp(1, usize::from(height.max(1))) as u16;
    let top = center
        .saturating_sub(count / 2)
        .min(height.saturating_sub(count));
    (top, count)
}

/// Brace around a block: the tip stays on the center row, which the block always covers.
pub fn block_brace(height: u16, center: u16, lines: usize) -> Brace {
    let (top, count) = block_rows(height, center, lines);
    Brace {
        top,
        bottom: top + count - 1,
        tip: center.clamp(top, top + count - 1),
        clipped_top: false,
        clipped_bottom: lines > usize::from(height),
    }
}

/// Glyphs for each brace row, as three cells: tip arm, spine, arm toward the child.
pub fn brace_glyphs(b: Brace) -> Vec<(u16, [char; 3])> {
    (b.top..=b.bottom)
        .map(|row| {
            let at_top = row == b.top;
            let at_bottom = row == b.bottom;
            let cells = if row == b.tip {
                let spine = match (at_top && !b.clipped_top, at_bottom && !b.clipped_bottom) {
                    (true, true) => '─',
                    (true, false) => '┬',
                    (false, true) => '┴',
                    (false, false) => '┤',
                };
                ['─', spine, if spine == '┤' { ' ' } else { '─' }]
            } else if at_top && !b.clipped_top {
                [' ', '╭', '─']
            } else if at_bottom && !b.clipped_bottom {
                [' ', '╰', '─']
            } else {
                [' ', '│', ' ']
            };
            (row, cells)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_flush_left_when_they_fit() {
        let placed = place_columns(&[10, 20], 60, 0);
        assert_eq!(
            placed,
            [
                Placed {
                    level: 0,
                    x: 0,
                    width: 10
                },
                Placed {
                    level: 1,
                    x: 13,
                    width: 20
                },
            ]
        );
    }

    #[test]
    fn leftmost_levels_scroll_off_first() {
        let placed = place_columns(&[20, 20, 20], 50, 2);
        assert_eq!(placed.iter().map(|p| p.level).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            placed[0].x, 0,
            "the first visible level starts at the left edge"
        );
    }

    #[test]
    fn levels_from_keep_from_are_never_dropped_but_shrink() {
        let placed = place_columns(&[30, 30, 30], 40, 1);
        assert_eq!(placed.iter().map(|p| p.level).collect::<Vec<_>>(), [1, 2]);
        let end = placed.last().map(|p| p.x + p.width).unwrap();
        assert!(
            end <= 40,
            "columns must stay inside the viewport, ended at {end}"
        );
    }

    #[test]
    fn visible_range_keeps_cursor_on_center_row() {
        // 100 entries, cursor 50, center row 5, height 11: entries 45..56
        assert_eq!(visible_range(100, 50, 5, 11), 45..56);
        // near the top, rows above the first entry stay empty
        assert_eq!(visible_range(100, 2, 5, 11), 0..8);
        // near the bottom
        assert_eq!(visible_range(10, 9, 5, 11), 4..10);
        assert_eq!(visible_range(0, 0, 5, 11), 0..0);
    }

    fn column(glyphs: &[(u16, [char; 3])], index: usize) -> String {
        glyphs.iter().map(|(_, c)| c[index]).collect()
    }

    #[test]
    fn brace_spans_the_whole_child_with_the_tip_on_the_center_row() {
        // 5 entries, cursor on the middle one: symmetrical brace around row 5
        let b = brace(11, 5, 5, 2);
        assert_eq!((b.top, b.bottom, b.tip), (3, 7, 5));
        let g = brace_glyphs(b);
        assert_eq!(column(&g, 1), "╭│┤│╰");
        assert_eq!(column(&g, 0), "  ─  ");
        assert_eq!(column(&g, 2), "─   ─");
    }

    #[test]
    fn brace_tip_on_first_or_last_entry_uses_tee_corners() {
        let g = brace_glyphs(brace(11, 5, 3, 0));
        assert_eq!(column(&g, 1), "┬│╰");
        let g = brace_glyphs(brace(11, 5, 3, 2));
        assert_eq!(column(&g, 1), "╭│┴");
    }

    #[test]
    fn single_entry_or_empty_child_gets_a_straight_line() {
        for len in [0, 1] {
            let g = brace_glyphs(brace(11, 5, len, 0));
            assert_eq!(g, [(5, ['─', '─', '─'])]);
        }
    }

    #[test]
    fn brace_ends_open_where_the_child_is_clipped_by_the_screen() {
        let b = brace(11, 5, 100, 50);
        assert_eq!((b.top, b.bottom), (0, 10));
        assert!(b.clipped_top && b.clipped_bottom);
        let g = brace_glyphs(b);
        assert_eq!(column(&g, 1), "│││││┤│││││");
    }

    #[test]
    fn the_tree_stays_inside_its_share_and_the_content_takes_the_rest() {
        let placed = place(&[20, 20, 20], true, 100, 2, 50);
        let levels: Vec<_> = placed.iter().map(|p| p.level).collect();
        assert_eq!(levels, [1, 2, 3], "only two folder columns fit in 50 cells");
        let content = placed.last().unwrap();
        assert_eq!(content.x, 20 + GUTTER + 20 + GUTTER);
        assert_eq!(content.x + content.width, 100);
        assert!(placed[1].x + placed[1].width <= 50);
    }

    #[test]
    fn without_content_the_tree_still_keeps_to_its_share() {
        let placed = place(&[30, 30, 30], false, 100, 2, 50);
        assert!(placed.iter().all(|p| p.x + p.width <= 50), "{placed:?}");
        let wide = place(&[30, 30, 30], false, 100, 2, 100);
        assert_eq!(wide.len(), 3);
    }

    #[test]
    fn the_focused_column_and_its_child_are_never_squeezed_by_the_share() {
        let placed = place(&[30, 30, 30], true, 100, 1, 30);
        let levels: Vec<_> = placed.iter().map(|p| p.level).collect();
        assert_eq!(levels, [1, 2, 3]);
        assert_eq!((placed[0].width, placed[1].width), (30, 30));
    }

    #[test]
    fn a_full_width_tree_still_leaves_the_content_its_minimum() {
        let placed = place(&[40, 40], true, 100, 1, 100);
        let content = placed.last().unwrap();
        assert!(content.width >= CONTENT_MIN, "{placed:?}");
        assert_eq!(content.x + content.width, 100);
    }

    #[test]
    fn short_columns_leave_the_content_more_than_its_share() {
        let placed = place(&[10], true, 100, 0, 50);
        assert_eq!(placed[1].x, 10 + GUTTER);
        assert_eq!(placed[1].width, 100 - 10 - GUTTER);
    }

    #[test]
    fn short_blocks_center_on_the_cursor_row_and_tall_ones_fill_the_height() {
        assert_eq!(block_rows(21, 10, 5), (8, 5));
        assert_eq!(block_rows(21, 10, 1), (10, 1));
        assert_eq!(block_rows(21, 10, 100), (0, 21));
        assert_eq!(
            block_rows(21, 2, 9),
            (0, 9),
            "near the top the block is pushed down to fit"
        );
        assert_eq!(block_rows(21, 19, 9), (12, 9));
        assert_eq!(block_rows(0, 0, 3), (0, 1));
    }

    #[test]
    fn block_brace_covers_the_block_with_the_tip_on_the_center_row() {
        let b = block_brace(21, 10, 5);
        assert_eq!((b.top, b.bottom, b.tip), (8, 12, 10));
        assert!(!b.clipped_bottom);
        let tall = block_brace(21, 10, 300);
        assert_eq!((tall.top, tall.bottom, tall.tip), (0, 20, 10));
        assert!(tall.clipped_bottom && !tall.clipped_top);
        let g = brace_glyphs(b);
        assert_eq!(g.iter().map(|(_, c)| c[1]).collect::<String>(), "╭│┤│╰");
    }
}
