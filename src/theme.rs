use ratatui::style::Color;

pub const BG: (u8, u8, u8) = (0x0d, 0x11, 0x17);
pub const FG: (u8, u8, u8) = (0xc9, 0xd1, 0xd9);
pub const DIM: (u8, u8, u8) = (0x6b, 0x76, 0x86);

const LEVELS: [(u8, u8, u8); 6] = [
    (0x5f, 0xb3, 0xff),
    (0x7e, 0xe7, 0x87),
    (0xf2, 0xcc, 0x60),
    (0xff, 0x9e, 0x64),
    (0xf4, 0x7f, 0xb5),
    (0xbb, 0x9a, 0xf7),
];

/// Color of a directory depth; depth is the absolute path depth so a directory keeps its color while scrolling.
pub fn level_color(depth: usize) -> (u8, u8, u8) {
    LEVELS[depth % LEVELS.len()]
}

/// `t` = 1.0 gives `a`, 0.0 gives `b`.
pub fn blend(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let mix = |x: u8, y: u8| (f32::from(x) * t + f32::from(y) * (1.0 - t)).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

pub fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Ansi256,
    Ansi16,
    /// No colour at all, from `NO_COLOR` or a dumb terminal.
    None,
}

impl Depth {
    pub fn parse(word: &str) -> Option<Option<Depth>> {
        Some(match word {
            "auto" => None,
            "truecolor" => Some(Depth::TrueColor),
            "256" => Some(Depth::Ansi256),
            "16" => Some(Depth::Ansi16),
            "none" => Some(Depth::None),
            _ => return None,
        })
    }

    /// Reads the usual environment variables: `NO_COLOR`, `COLORTERM` and `TERM`.
    pub fn detect(var: impl Fn(&str) -> Option<String>) -> Depth {
        if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Depth::None;
        }
        let term = var("TERM").unwrap_or_default();
        if term == "dumb" {
            return Depth::None;
        }
        let colorterm = var("COLORTERM").unwrap_or_default().to_lowercase();
        if colorterm == "truecolor" || colorterm == "24bit" {
            return Depth::TrueColor;
        }
        if term.contains("256color") {
            return Depth::Ansi256;
        }
        Depth::Ansi16
    }
}

const ANSI16: [((u8, u8, u8), Color); 16] = [
    ((0, 0, 0), Color::Black),
    ((205, 0, 0), Color::Red),
    ((0, 205, 0), Color::Green),
    ((205, 205, 0), Color::Yellow),
    ((0, 0, 238), Color::Blue),
    ((205, 0, 205), Color::Magenta),
    ((0, 205, 205), Color::Cyan),
    ((229, 229, 229), Color::Gray),
    ((127, 127, 127), Color::DarkGray),
    ((255, 0, 0), Color::LightRed),
    ((0, 255, 0), Color::LightGreen),
    ((255, 255, 0), Color::LightYellow),
    ((92, 92, 255), Color::LightBlue),
    ((255, 0, 255), Color::LightMagenta),
    ((0, 255, 255), Color::LightCyan),
    ((255, 255, 255), Color::White),
];

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2) as u32;
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// The closest colour of the xterm 256-colour palette: the 6x6x6 cube or the grey ramp.
pub fn to_256(c: (u8, u8, u8)) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| {
        (0..6)
            .min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(v)).abs())
            .unwrap_or(0)
    };
    let (r, g, b) = (level(c.0), level(c.1), level(c.2));
    let cube = (LEVELS[r], LEVELS[g], LEVELS[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;
    let average = (u32::from(c.0) + u32::from(c.1) + u32::from(c.2)) / 3;
    let step = ((average.saturating_sub(8)) / 10).min(23) as u8;
    let grey = 8 + 10 * step;
    if distance(c, (grey, grey, grey)) < distance(c, cube) {
        232 + step
    } else {
        cube_index as u8
    }
}

pub fn to_16(c: (u8, u8, u8)) -> Color {
    ANSI16
        .iter()
        .min_by_key(|(rgb, _)| distance(c, *rgb))
        .map_or(Color::Reset, |(_, color)| *color)
}

/// Converts a frame drawn in full RGB to what the terminal can show. Without colours, anything
/// drawn on a coloured background, such as the cursor row or a selection, turns into reverse video.
pub fn adapt(buf: &mut ratatui::buffer::Buffer, depth: Depth) {
    if depth == Depth::TrueColor {
        return;
    }
    let background = rgb(BG);
    for cell in buf.content.iter_mut() {
        match depth {
            Depth::TrueColor => {}
            Depth::Ansi256 => {
                cell.fg = convert(cell.fg, |c| Color::Indexed(to_256(c)));
                cell.bg = convert(cell.bg, |c| Color::Indexed(to_256(c)));
            }
            Depth::Ansi16 => {
                cell.fg = convert(cell.fg, to_16);
                cell.bg = if cell.bg == background {
                    Color::Reset
                } else {
                    convert(cell.bg, to_16)
                };
            }
            Depth::None => {
                let highlighted = !matches!(cell.bg, Color::Reset) && cell.bg != background;
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
                if highlighted {
                    cell.modifier.insert(ratatui::style::Modifier::REVERSED);
                }
            }
        }
    }
}

fn convert(color: Color, map: impl Fn((u8, u8, u8)) -> Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => map((r, g, b)),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn depth_follows_the_environment() {
        assert_eq!(
            Depth::detect(env(&[("COLORTERM", "truecolor"), ("TERM", "xterm")])),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::detect(env(&[("TERM", "xterm-256color")])),
            Depth::Ansi256
        );
        assert_eq!(Depth::detect(env(&[("TERM", "linux")])), Depth::Ansi16);
        assert_eq!(Depth::detect(env(&[("TERM", "dumb")])), Depth::None);
        assert_eq!(
            Depth::detect(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])),
            Depth::None,
            "NO_COLOR wins"
        );
        assert_eq!(
            Depth::detect(env(&[("NO_COLOR", ""), ("TERM", "linux")])),
            Depth::Ansi16,
            "an empty NO_COLOR is ignored"
        );
    }

    #[test]
    fn colours_map_to_the_nearest_palette_entry() {
        assert_eq!(to_256((0, 0, 0)), 16);
        assert_eq!(to_256((255, 255, 255)), 231);
        assert_eq!(to_256((255, 0, 0)), 196);
        assert_eq!(to_256((128, 128, 128)), 244, "greys use the grey ramp");
        assert_eq!(to_16((250, 10, 10)), Color::LightRed);
        assert_eq!(to_16((0x5f, 0xb3, 0xff)), Color::LightBlue);
    }

    #[test]
    fn every_level_colour_stays_distinct_in_256_colours() {
        let mapped: std::collections::HashSet<u8> =
            (0..6).map(|d| to_256(level_color(d))).collect();
        assert_eq!(mapped.len(), 6);
    }

    #[test]
    fn without_colour_highlights_become_reverse_video() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf[(0, 0)].set_bg(rgb(BG)).set_fg(rgb(FG));
        buf[(1, 0)].set_bg(rgb(level_color(0))).set_fg(rgb(BG));
        adapt(&mut buf, Depth::None);
        assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED));
        assert_eq!(buf[(1, 0)].fg, Color::Reset);
    }

    #[test]
    fn sixteen_colours_leave_the_terminal_background_alone() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        buf[(0, 0)].set_bg(rgb(BG));
        buf[(1, 0)].set_bg(rgb(level_color(1)));
        adapt(&mut buf, Depth::Ansi16);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
        assert_ne!(buf[(1, 0)].bg, Color::Reset);
    }
}
