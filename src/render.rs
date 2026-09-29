use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    app::{App, OverlayView},
    layout::{self, Col, Placed},
    model::{Entry, FilePreview, Kind, Level, Load, PreviewState},
    preview::{Content, Span},
    theme::{self, BG, DIM, FG},
};

const MIN_WIDTH: u16 = 16;
const MAX_WIDTH: u16 = 40;
const META_WIDTH: u16 = 6;
const WARN: (u8, u8, u8) = (0xff, 0x9e, 0x64);
/// A listing faster than this never flashes a loading label.
const LOADING_LABEL_DELAY: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The level holding the cursor.
    Focused,
    /// A level on the path to the focus; its selected entry is the one that was opened.
    Ancestor,
    /// The child of the directory under the cursor.
    Preview,
}

pub fn render(app: &App, area: Rect, buf: &mut Buffer) {
    fill(buf, area, Style::new().bg(theme::rgb(BG)));
    if area.height < 3 {
        return;
    }
    let tree = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    let center = tree.height / 2;

    let focus = app.tree().focus();
    let levels = app.tree().levels();
    let preview = app.tree().preview();
    let mut cols: Vec<Col> = levels
        .iter()
        .map(|l| Col::rigid(natural_width(l)))
        .collect();
    cols.extend(preview.map(preview_col));
    let placed = layout::place_columns(&cols, tree.width, focus);

    for p in &placed {
        if let (true, Some(preview)) = (p.level >= levels.len(), preview) {
            draw_preview(buf, tree, center, *p, preview);
            continue;
        }
        let role = match p.level.cmp(&focus) {
            std::cmp::Ordering::Less => Role::Ancestor,
            std::cmp::Ordering::Equal => Role::Focused,
            std::cmp::Ordering::Greater => Role::Preview,
        };
        let marks = Marks {
            selection: app.selection(),
            visual: (role == Role::Focused)
                .then(|| app.visual_range())
                .flatten(),
        };
        draw_column(buf, tree, center, *p, &levels[p.level], role, &marks);
    }
    for pair in placed.windows(2) {
        let (spec, color) = match (levels.get(pair[1].level), preview) {
            (Some(child), _) => (
                layout::brace(tree.height, center, child.entries.len(), child.cursor),
                level_color(child),
            ),
            (None, Some(preview)) => (
                layout::block_brace(tree.height, center, preview_lines(preview)),
                preview_color(preview),
            ),
            (None, None) => continue,
        };
        let style = Style::new().fg(theme::rgb(color));
        let x = tree.x + pair[0].x + pair[0].width;
        for (row, cells) in layout::brace_glyphs(spec) {
            for (i, ch) in cells.into_iter().enumerate() {
                let cell_x = x + i as u16;
                if cell_x < tree.right() {
                    buf[(cell_x, tree.y + row)].set_char(ch).set_style(style);
                }
            }
        }
    }

    buf.set_stringn(
        area.x + 1,
        area.y,
        tilde(app.tree().current_dir()),
        usize::from(area.width.saturating_sub(2)),
        Style::new().fg(theme::rgb(DIM)),
    );
    if app.tree().show_hidden() {
        let label = "dotfiles shown";
        let x = area
            .right()
            .saturating_sub(label.width() as u16 + 1)
            .max(area.x);
        buf.set_string(x, area.y, label, Style::new().fg(theme::rgb(DIM)));
    }
    draw_footer(buf, area, app);
    if let Some(overlay) = app.overlay() {
        draw_overlay(buf, tree, overlay);
    }
}

fn draw_footer(buf: &mut Buffer, area: Rect, app: &App) {
    let y = area.y + area.height - 1;
    let width = usize::from(area.width.saturating_sub(2));
    if let Some(prompt) = app.prompt_view() {
        let text = format!("{}{}", prompt.label, prompt.text);
        buf.set_stringn(area.x + 1, y, &text, width, Style::new().fg(theme::rgb(FG)));
        let cursor_x = area.x + 1 + (prompt.label.width() + prompt.cursor_col) as u16;
        if cursor_x < area.right() {
            let under = if prompt.cursor_col >= prompt.text.width() {
                " "
            } else {
                ""
            };
            let cell = &mut buf[(cursor_x, y)];
            if !under.is_empty() {
                cell.set_char(' ');
            }
            cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
        }
        return;
    }
    let running = app.running().map(|(label, percent)| match percent {
        Some(p) => format!("{label}… {p}%  (Esc cancels)"),
        None => format!("{label}…  (Esc cancels)"),
    });
    let conflict = app.conflict_prompt();
    let (text, color) = match (&conflict, &app.message, &running) {
        (Some(question), _, _) => (question.as_str(), FG),
        (None, Some(message), _) => (message.as_str(), WARN),
        (None, None, Some(progress)) => (progress.as_str(), FG),
        (None, None, None) if app.is_visual() => {
            ("-- VISUAL --   d trash   y yank   x cut   Esc leaves", FG)
        }
        (None, None, None) => (
            "j/k move  l enter  h back  / search  : command  d y x p  u undo  ? help  q quit",
            DIM,
        ),
    };
    let pending = app.pending();
    let reserved = if pending.is_empty() {
        0
    } else {
        pending.width() + 3
    };
    buf.set_stringn(
        area.x + 1,
        y,
        text,
        width.saturating_sub(reserved),
        Style::new().fg(theme::rgb(color)),
    );
    if !pending.is_empty() {
        let x = area
            .right()
            .saturating_sub(pending.width() as u16 + 2)
            .max(area.x);
        buf.set_string(x, y, &pending, Style::new().fg(theme::rgb(FG)));
    }
}

/// A read-only list drawn in a box over the tree.
fn draw_overlay(buf: &mut Buffer, tree: Rect, overlay: OverlayView) {
    let content = overlay.lines;
    let hint = format!("{}   (j/k scroll, q close)", overlay.title);
    let inner_w = content
        .iter()
        .map(|l| l.width())
        .max()
        .unwrap_or(0)
        .max(hint.width())
        + 4;
    let width = (inner_w as u16 + 2).min(tree.width);
    let height = (content.len() as u16 + 4).min(tree.height);
    let area = Rect {
        x: tree.x + (tree.width - width) / 2,
        y: tree.y + (tree.height - height) / 2,
        width,
        height,
    };
    let border = Style::new().fg(theme::rgb(DIM)).bg(theme::rgb(BG));
    fill(buf, area, Style::new().bg(theme::rgb(BG)));
    let bottom = area.y + area.height - 1;
    for x in area.x..area.right() {
        buf[(x, area.y)].set_char('─').set_style(border);
        buf[(x, bottom)].set_char('─').set_style(border);
    }
    for y in area.y..=bottom {
        buf[(area.x, y)].set_char('│').set_style(border);
        buf[(area.right() - 1, y)].set_char('│').set_style(border);
    }
    for (x, y, ch) in [
        (area.x, area.y, '╭'),
        (area.right() - 1, area.y, '╮'),
        (area.x, bottom, '╰'),
        (area.right() - 1, bottom, '╯'),
    ] {
        buf[(x, y)].set_char(ch).set_style(border);
    }
    let text_w = usize::from(area.width.saturating_sub(4));
    buf.set_stringn(
        area.x + 2,
        area.y + 1,
        &hint,
        text_w,
        Style::new().fg(theme::rgb(DIM)),
    );
    let visible = usize::from(area.height.saturating_sub(4));
    let first = overlay.scroll.min(content.len().saturating_sub(visible));
    for (i, line) in content.iter().skip(first).take(visible).enumerate() {
        let y = area.y + 3 + i as u16;
        buf.set_stringn(area.x + 2, y, line, text_w, Style::new().fg(theme::rgb(FG)));
    }
}

fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_char(' ').set_style(style);
        }
    }
}

const PREVIEW_MIN_WIDTH: u16 = 24;
const PREVIEW_MAX_WIDTH: u16 = 100;
const PREVIEW_MEASURED_LINES: usize = 200;

fn preview_color(preview: &FilePreview) -> (u8, u8, u8) {
    theme::level_color(preview.path.components().count())
}

fn preview_lines(preview: &FilePreview) -> usize {
    match &preview.state {
        PreviewState::Ready(content) => content.lines.len().max(1),
        _ => 1,
    }
}

fn gutter_width(content: &Content) -> u16 {
    if !content.numbered {
        return 0;
    }
    content.lines.len().to_string().len().max(2) as u16 + 1
}

fn preview_col(preview: &FilePreview) -> Col {
    let natural = match &preview.state {
        PreviewState::Ready(content) => {
            let longest = content
                .lines
                .iter()
                .take(PREVIEW_MEASURED_LINES)
                .map(|l| l.0.iter().map(|s| s.text.width()).sum::<usize>())
                .max()
                .unwrap_or(0) as u16;
            (longest + gutter_width(content) + 2).clamp(30, PREVIEW_MAX_WIDTH)
        }
        _ => 30,
    };
    Col {
        natural,
        min: PREVIEW_MIN_WIDTH.min(natural),
    }
}

fn draw_preview(buf: &mut Buffer, tree: Rect, center: u16, p: Placed, preview: &FilePreview) {
    let color = preview_color(preview);
    let x = tree.x + p.x;
    let note = |buf: &mut Buffer, text: &str| {
        let style = Style::new().fg(theme::rgb(theme::blend(color, BG, 0.6)));
        buf.set_stringn(
            x + 1,
            tree.y + center,
            text,
            usize::from(p.width.saturating_sub(1)),
            style,
        );
    };
    let content = match &preview.state {
        PreviewState::Loading { since } if since.elapsed() < LOADING_LABEL_DELAY => return,
        PreviewState::Loading { .. } => return note(buf, "(loading…)"),
        PreviewState::Failed(message) => return note(buf, &format!("({message})")),
        PreviewState::Ready(content) if content.lines.is_empty() => {
            return note(buf, "(empty file)");
        }
        PreviewState::Ready(content) => content,
    };
    let total = content.lines.len();
    let (top, count) = layout::block_rows(tree.height, center, total);
    let scroll = preview.scroll.min(total.saturating_sub(usize::from(count)));
    let gutter = gutter_width(content).min(p.width.saturating_sub(8));
    let digits = usize::from(gutter.saturating_sub(1));
    for i in 0..usize::from(count) {
        let y = tree.y + top + i as u16;
        let line = &content.lines[scroll + i];
        if gutter > 0 {
            let number = format!("{:>digits$} ", scroll + i + 1);
            buf.set_stringn(
                x + 1,
                y,
                number,
                usize::from(gutter),
                Style::new().fg(theme::rgb(DIM)),
            );
        }
        let mut cx = x + 1 + gutter;
        let end = x + p.width;
        for Span {
            text,
            color,
            bold,
            italic,
        } in &line.0
        {
            if cx >= end {
                break;
            }
            let mut style = Style::new().fg(theme::rgb(*color));
            if *bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if *italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            (cx, _) = buf.set_stringn(cx, y, text, usize::from(end - cx), style);
        }
    }
}

fn level_color(level: &Level) -> (u8, u8, u8) {
    theme::level_color(level.dir.components().count())
}

fn natural_width(level: &Level) -> u16 {
    let longest = level
        .entries
        .iter()
        .map(|e| display(e).width())
        .max()
        .unwrap_or("(empty)".len()) as u16;
    (longest + META_WIDTH + 3).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// What the rows of a column need to know beyond the listing itself.
struct Marks<'a> {
    selection: &'a std::collections::BTreeSet<std::path::PathBuf>,
    /// Rows covered by visual mode. Only the focused column has any.
    visual: Option<std::ops::RangeInclusive<usize>>,
}

fn draw_column(
    buf: &mut Buffer,
    tree: Rect,
    center: u16,
    p: Placed,
    level: &Level,
    role: Role,
    marks: &Marks,
) {
    let color = level_color(level);
    let x = tree.x + p.x;

    if level.entries.is_empty() {
        let text = match level.load {
            Load::Failed(kind) => format!("({})", kind.to_string().to_lowercase()),
            Load::Loading { since } if since.elapsed() < LOADING_LABEL_DELAY => return,
            Load::Loading { .. } => "(loading…)".to_string(),
            Load::Ready => "(empty)".to_string(),
        };
        let style = Style::new().fg(theme::rgb(theme::blend(color, BG, 0.5)));
        buf.set_stringn(x + 1, tree.y + center, text, usize::from(p.width), style);
        return;
    }

    for i in layout::visible_range(level.entries.len(), level.cursor, center, tree.height) {
        let row = layout::row_y(center, level.cursor, i) as u16;
        let y = tree.y + row;
        let entry = &level.entries[i];
        let is_cursor = i == level.cursor;
        let distance = i.abs_diff(level.cursor);

        let base = if entry.is_dir() {
            color
        } else {
            theme::blend(color, FG, 0.55)
        };
        let fade = (1.0 - distance as f32 * 0.14).max(0.3);
        let mut style = Style::new().fg(theme::rgb(theme::blend(base, BG, fade)));
        if is_cursor {
            let bg = match role {
                Role::Focused => color,
                Role::Ancestor => theme::blend(color, BG, 0.24),
                Role::Preview => theme::blend(color, BG, 0.12),
            };
            style = Style::new().bg(theme::rgb(bg)).fg(theme::rgb(base));
            if role == Role::Focused {
                style = style.fg(theme::rgb(BG)).add_modifier(Modifier::BOLD);
            }
            fill(
                buf,
                Rect {
                    x,
                    y,
                    width: p.width,
                    height: 1,
                },
                style,
            );
        } else if marks.visual.as_ref().is_some_and(|r| r.contains(&i)) {
            style = style.bg(theme::rgb(theme::blend(color, BG, 0.24)));
            fill(
                buf,
                Rect {
                    x,
                    y,
                    width: p.width,
                    height: 1,
                },
                style,
            );
        }
        if marks.selection.contains(&level.dir.join(&entry.name)) {
            let marker = if is_cursor && role == Role::Focused {
                style
            } else {
                style.fg(theme::rgb(color)).add_modifier(Modifier::BOLD)
            };
            buf.set_string(x, y, "●", marker);
        }

        let meta = meta(entry);
        let name_width = usize::from(p.width.saturating_sub(META_WIDTH + 3));
        buf.set_stringn(
            x + 1,
            y,
            fit(&display(entry), name_width),
            name_width,
            style,
        );
        let meta_style = if is_cursor && role == Role::Focused {
            style
        } else {
            style.fg(theme::rgb(theme::blend(DIM, BG, fade)))
        };
        let meta_x = (x + p.width).saturating_sub(1 + meta.width() as u16).max(x);
        buf.set_string(meta_x, y, meta, meta_style);
    }
}

fn display(entry: &Entry) -> String {
    let name = entry.display_name();
    match &entry.kind {
        Kind::Dir | Kind::Symlink { to_dir: true, .. } => format!("{name}/"),
        Kind::Symlink { target, .. } => format!("{name} → {}", target.display()),
        _ => name,
    }
}

fn meta(entry: &Entry) -> String {
    match entry.kind {
        Kind::File => human_size(entry.size),
        _ => String::new(),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 || size >= 10.0 {
        format!("{:.0}{}", size, UNITS[unit])
    } else {
        format!("{:.1}{}", size, UNITS[unit])
    }
}

fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

fn tilde(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => match shown.strip_prefix(&home) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
            _ => shown,
        },
        _ => shown,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ratatui::{Terminal, backend::TestBackend};

    use crate::keys::{Key, Keymap};

    use super::*;

    fn rows(app: &App, w: u16, h: u16) -> (Vec<String>, Buffer) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(app, f.area(), f.buffer_mut()))
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let lines = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        (lines, buf)
    }

    fn open(path: &std::path::Path) -> App {
        let mut app = App::new(path.to_path_buf(), Keymap::default());
        app.tree_mut().settle();
        app
    }

    fn keys(app: &mut App, text: &str) {
        for key in Key::parse_seq(text).unwrap() {
            app.press(key);
            app.settle();
        }
    }

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        fs::create_dir_all(r.join("alpha/inner")).unwrap();
        fs::create_dir_all(r.join("beta")).unwrap();
        fs::write(r.join("alpha/one.txt"), "x").unwrap();
        fs::write(r.join("alpha/two.txt"), "xx").unwrap();
        fs::write(r.join("file.md"), "hello").unwrap();
        tmp
    }

    #[test]
    fn selected_entries_of_all_levels_share_one_row_joined_by_a_brace() {
        let tmp = fixture();
        let app = open(tmp.path());
        let (lines, _) = rows(&app, 60, 11);
        // tree area is rows 1..10, center row is 1 + 9 / 2 = 5
        let center = &lines[5];
        assert!(center.contains("alpha/"), "{center:?}");
        assert!(center.contains("inner/"), "{center:?}");
        // inner/ is the first entry of alpha, so the tip is a tee opening downward
        assert!(
            center.contains("─┬─"),
            "brace tip missing on the center row: {center:?}"
        );
        assert!(lines[6].contains('│'), "brace spine missing: {lines:#?}");
    }

    #[test]
    fn brace_tip_is_a_left_tee_when_the_child_cursor_is_in_the_middle() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "l");
        keys(&mut app, "j");
        let (lines, _) = rows(&app, 90, 11);
        assert!(lines[5].contains("─┤"), "{lines:#?}");
        assert!(
            lines[4].contains('╭') && lines[6].contains('╰'),
            "{lines:#?}"
        );
    }

    #[test]
    fn moving_down_slides_the_list_and_swaps_the_child() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "j");
        let (lines, _) = rows(&app, 60, 11);
        assert!(lines[5].contains("beta/"), "{:?}", lines[5]);
        assert!(lines[4].contains("alpha/"), "{:?}", lines[4]);
        assert!(lines.iter().all(|l| !l.contains("one.txt")), "{lines:#?}");
        assert!(lines[5].contains("(empty)"), "{:?}", lines[5]);
    }

    #[test]
    fn levels_are_colored_by_depth_and_cursor_row_is_filled() {
        let tmp = fixture();
        let app = open(tmp.path());
        let (lines, buf) = rows(&app, 60, 11);
        let x = lines[5].find("alpha/").unwrap();
        let x = lines[5][..x].chars().count() as u16;
        let depth = tmp.path().components().count();
        let expect = theme::rgb(theme::level_color(depth));
        assert_eq!(buf[(x, 5)].bg, expect);
        let child = lines[5].find("inner/").unwrap();
        let child_x = lines[5][..child].chars().count() as u16;
        let child_color = theme::rgb(theme::level_color(depth + 1));
        assert_ne!(child_color, expect);
        assert_eq!(buf[(child_x, 5)].fg, child_color);
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let tmp = fixture();
        let app = open(tmp.path());
        for (w, h) in [(1, 1), (5, 2), (10, 3), (20, 4), (3, 40)] {
            rows(&app, w, h);
        }
    }

    #[test]
    fn a_deep_path_in_a_narrow_terminal_keeps_the_focus_visible_inside_the_screen() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp
            .path()
            .join("aaaaaaaaaaaa/bbbbbbbbbbbb/cccccccccccc/dddddddddddd/eeeeeeeeeeee");
        fs::create_dir_all(deep.join("leaf")).unwrap();
        fs::write(deep.join("readme.txt"), "x").unwrap();
        let mut app = open(tmp.path());
        for _ in 0..5 {
            keys(&mut app, "l");
        }
        assert_eq!(app.tree().focus(), 5);
        let (lines, _) = rows(&app, 50, 9);
        let center = &lines[4];
        assert!(
            center.starts_with(" leaf/"),
            "focused level must be the leftmost column: {center:?}"
        );
        assert!(
            center.contains("(empty)"),
            "the preview of leaf/ stays visible: {center:?}"
        );
        assert!(
            !center.contains("aaaaaaaaaaaa/"),
            "ancestors that do not fit scroll off: {center:?}"
        );
        let (wide, _) = rows(&app, 70, 9);
        assert!(
            wide[4].contains("eeeeeeeeeeee/"),
            "the parent returns once it fits: {:?}",
            wide[4]
        );
        assert!(lines.iter().all(|l| l.chars().count() == 50));
    }

    #[test]
    fn the_first_column_is_flush_left() {
        let tmp = fixture();
        let app = open(tmp.path());
        let (lines, buf) = rows(&app, 60, 11);
        assert!(lines[5].starts_with(" alpha/"), "{:?}", lines[5]);
        assert_ne!(
            buf[(0, 5)].bg,
            theme::rgb(BG),
            "cursor highlight starts at column 0"
        );
    }

    #[test]
    fn a_pending_listing_renders_blank_instead_of_flashing_a_label() {
        let tmp = fixture();
        let app = App::new(tmp.path().to_path_buf(), Keymap::default());
        let (lines, _) = rows(&app, 60, 11);
        assert!(lines[5].trim().is_empty(), "{:?}", lines[5]);
    }

    fn footer(lines: &[String]) -> &str {
        lines.last().unwrap().trim()
    }

    #[test]
    fn typing_a_search_shows_the_prompt_and_jumps_the_cursor_row_to_the_match() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/file");
        let (lines, _) = rows(&app, 60, 11);
        assert_eq!(footer(&lines), "/file");
        assert!(
            lines[5].contains("file.md"),
            "match sits on the center row: {:?}",
            lines[5]
        );
    }

    #[test]
    fn a_failed_search_reports_in_the_footer_after_enter() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/zzz<cr>");
        let (lines, _) = rows(&app, 60, 11);
        assert_eq!(footer(&lines), "pattern not found: zzz");
    }

    #[test]
    fn the_prompt_cursor_is_drawn_as_a_reversed_cell() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "/ab<left>");
        let (lines, buf) = rows(&app, 60, 11);
        assert_eq!(footer(&lines), "/ab");
        assert!(
            buf[(3, 10)].modifier.contains(Modifier::REVERSED),
            "cursor sits on b"
        );
        assert!(!buf[(2, 10)].modifier.contains(Modifier::REVERSED));
        keys(&mut app, "<end>");
        let (_, buf) = rows(&app, 60, 11);
        assert!(
            buf[(4, 10)].modifier.contains(Modifier::REVERSED),
            "cursor sits past the end"
        );
    }

    #[test]
    fn a_pending_count_shows_at_the_right_edge_of_the_footer() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "12g");
        let (lines, _) = rows(&app, 60, 11);
        assert!(lines[10].trim_end().ends_with("12g"), "{:?}", lines[10]);
    }

    #[test]
    fn the_help_overlay_lists_the_live_bindings_and_follows_rebinding() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "?");
        let (lines, _) = rows(&app, 80, 30);
        let text = lines.join("\n");
        assert!(text.contains("move down"), "{text}");
        assert!(text.contains("gg"), "{text}");
        assert!(text.contains("╭") && text.contains("╯"), "{text}");
        keys(&mut app, "q");
        let (lines, _) = rows(&app, 80, 30);
        assert!(!lines.join("\n").contains("move down"));
    }

    #[test]
    fn the_help_overlay_fits_a_small_terminal() {
        let tmp = fixture();
        let mut app = open(tmp.path());
        keys(&mut app, "?");
        for (w, h) in [(20, 6), (40, 9), (5, 3), (120, 60)] {
            rows(&app, w, h);
        }
    }

    fn numbered_file(dir: &std::path::Path, name: &str, lines: usize) {
        let body: String = (1..=lines).map(|i| format!("row {i}\n")).collect();
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn a_file_under_the_cursor_is_previewed_beside_a_brace_with_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("hello.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let app = open(tmp.path());
        let (lines, _) = rows(&app, 80, 11);
        assert!(lines[5].contains("hello.txt"), "{lines:#?}");
        assert!(
            lines[4].contains("1 alpha") || lines.iter().any(|l| l.contains(" 1 alpha")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.contains(" 3 gamma")), "{lines:#?}");
        let tip = lines
            .iter()
            .position(|l| l.contains("─┤"))
            .expect("brace tip");
        assert!(
            lines[tip].contains("hello.txt") || lines[tip].contains("2 beta"),
            "tip is on the center row: {lines:#?}"
        );
        assert_eq!(tip, 5);
    }

    #[test]
    fn a_long_file_fills_the_height_and_its_brace_stays_open_at_the_bottom() {
        let tmp = tempfile::tempdir().unwrap();
        numbered_file(tmp.path(), "long.txt", 200);
        let app = open(tmp.path());
        let (lines, _) = rows(&app, 80, 15);
        let tree_rows = &lines[1..14];
        assert!(tree_rows[0].contains("row 1"), "{lines:#?}");
        assert!(tree_rows[12].contains("row 13"), "{lines:#?}");
        let spine: Vec<_> = tree_rows
            .iter()
            .map(|l| l.chars().any(|c| "╭│┤┬".contains(c)))
            .collect();
        assert!(
            spine.iter().all(|&b| b),
            "the brace covers every row: {lines:#?}"
        );
        assert!(
            !tree_rows[12].contains('╰'),
            "open end where content continues: {lines:#?}"
        );
    }

    #[test]
    fn capital_j_and_k_scroll_the_preview() {
        let tmp = tempfile::tempdir().unwrap();
        numbered_file(tmp.path(), "long.txt", 200);
        let mut app = open(tmp.path());
        app.set_viewport(13);
        keys(&mut app, "10J");
        let (lines, _) = rows(&app, 80, 15);
        assert!(lines[1].contains("row 11"), "{lines:#?}");
        keys(&mut app, "3K");
        let (lines, _) = rows(&app, 80, 15);
        assert!(lines[1].contains("row 8"), "{lines:#?}");
        keys(&mut app, "500J");
        let (lines, _) = rows(&app, 80, 15);
        assert!(
            lines[13].contains("row 200"),
            "the last page stays full: {lines:#?}"
        );
        assert!(lines[1].contains("row 188"), "{lines:#?}");
    }

    #[test]
    fn preview_shrinks_to_the_screen_instead_of_hiding_the_directory_columns() {
        let tmp = fixture();
        let wide = "x".repeat(300);
        fs::write(tmp.path().join("alpha/wide.txt"), &wide).unwrap();
        let mut app = open(tmp.path());
        keys(&mut app, "l");
        keys(&mut app, "G");
        let (lines, _) = rows(&app, 70, 11);
        assert!(lines[5].contains("wide.txt"), "{:?}", lines[5]);
        assert!(
            lines[5].contains("alpha/"),
            "parent column is kept: {:?}",
            lines[5]
        );
        assert!(lines.iter().all(|l| l.chars().count() == 70));
    }

    #[test]
    fn syntax_colors_reach_the_screen() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn main() { let x = 1; }\n").unwrap();
        let app = open(tmp.path());
        let (lines, buf) = rows(&app, 80, 11);
        let row = lines.iter().position(|l| l.contains("fn main")).unwrap();
        let col = lines[row].find("fn main").unwrap();
        let fg_fn = buf[(col as u16, row as u16)].fg;
        let fg_main = buf[(col as u16 + 3, row as u16)].fg;
        assert_ne!(fg_fn, fg_main, "keyword and identifier differ");
    }

    #[test]
    fn a_failed_preview_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("bad.zip"), b"not a zip").unwrap();
        let app = open(tmp.path());
        let (lines, _) = rows(&app, 80, 11);
        assert!(
            lines[5].contains("bad.zip") && lines[5].contains('('),
            "{lines:#?}"
        );
    }
}
