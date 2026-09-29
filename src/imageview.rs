//! Draws decoded images in the preview column with the best method the terminal offers: kitty,
//! sixel or iTerm2 graphics, or else quadrant block characters that any terminal can show.

use std::{cell::RefCell, path::PathBuf, sync::Arc};

use crate::termquery::Answers;

use image::DynamicImage;
use image::imageops::FilterType;
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
    style::Color,
    widgets::Widget,
};
use ratatui_image::{
    FontSize, Image, Resize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};

/// A decoded image, shared between the preview cache and the screen.
#[derive(Clone)]
pub struct ImageData(pub Arc<DynamicImage>);

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({}x{})", self.0.width(), self.0.height())
    }
}

impl PartialEq for ImageData {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ImageData {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Ask the terminal what it supports.
    Auto,
    Kitty,
    Sixel,
    Iterm2,
    /// Quadrant block characters: 2x2 pixels a cell, works in any terminal with a normal font.
    Blocks,
    /// Upper half blocks: 1x2 pixels a cell, for fonts without quadrant characters.
    Halfblocks,
    Off,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        Some(match text {
            "auto" => Mode::Auto,
            "kitty" => Mode::Kitty,
            "sixel" => Mode::Sixel,
            "iterm2" => Mode::Iterm2,
            "blocks" => Mode::Blocks,
            "halfblocks" => Mode::Halfblocks,
            "off" => Mode::Off,
            _ => return None,
        })
    }
}

/// How pictures end up on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Graphics(ProtocolType),
    Blocks(BlockKind),
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Quadrants,
    Halves,
}

/// Picks the drawing method from the config, the terminal's answers and its environment variables.
/// Returns the method and a short account of why, for `:images`.
pub fn choose(
    mode: Mode,
    answers: Option<&Answers>,
    var: impl Fn(&str) -> Option<String>,
) -> (Method, String) {
    let forced = |method, what: &str| (method, format!("{what}, set in the config"));
    match mode {
        Mode::Off => return forced(Method::Off, "off"),
        Mode::Kitty => return forced(Method::Graphics(ProtocolType::Kitty), "kitty graphics"),
        Mode::Sixel => return forced(Method::Graphics(ProtocolType::Sixel), "sixel"),
        Mode::Iterm2 => return forced(Method::Graphics(ProtocolType::Iterm2), "iTerm2 images"),
        Mode::Blocks => return forced(Method::Blocks(BlockKind::Quadrants), "quadrant blocks"),
        Mode::Halfblocks => return forced(Method::Blocks(BlockKind::Halves), "half blocks"),
        Mode::Auto => {}
    }
    let get = |name: &str| var(name).unwrap_or_default();
    let term = get("TERM");
    let program = get("TERM_PROGRAM");
    let in_tmux = var("TMUX").is_some() || term.starts_with("tmux") || term.starts_with("screen");
    // The image library wraps its output for tmux only when TERM says tmux.
    let tmux_wrapped = term.starts_with("tmux") || program == "tmux";
    let blocks = |why: &str| {
        (
            Method::Blocks(BlockKind::Quadrants),
            format!("quadrant blocks, {why}"),
        )
    };
    if in_tmux && !tmux_wrapped {
        return blocks("because pictures cannot pass through this tmux or screen setup");
    }
    let empty = Answers::default();
    let answers = answers.unwrap_or(&empty);
    let name = answers.name.clone().unwrap_or_default().to_lowercase();
    let heard = if answers.complete {
        match &answers.name {
            Some(name) => format!("the terminal says it is {name}"),
            None => "the terminal answered".to_string(),
        }
    } else {
        "the terminal did not answer".to_string()
    };
    let graphics = |p, what: &str| (Method::Graphics(p), format!("{what}: {heard}"));
    let konsole = name.contains("konsole") || var("KONSOLE_VERSION").is_some();
    let wezterm = name.contains("wezterm") || program.contains("WezTerm");
    // Konsole and WezTerm accept kitty graphics but not the unicode placeholders it is drawn with here.
    if konsole {
        return graphics(ProtocolType::Sixel, "sixel");
    }
    if wezterm || name.contains("iterm2") || program.contains("iTerm") || program.contains("mintty")
    {
        return graphics(ProtocolType::Iterm2, "iTerm2 images");
    }
    if answers.kitty || term == "xterm-kitty" || term == "xterm-ghostty" || program == "ghostty" {
        return graphics(ProtocolType::Kitty, "kitty graphics");
    }
    // Inside tmux the DA1 answer comes from tmux itself, not from the terminal the pictures would reach.
    if (answers.sixel && !in_tmux)
        || term.starts_with("foot")
        || term.starts_with("mlterm")
        || program.contains("contour")
    {
        return graphics(ProtocolType::Sixel, "sixel");
    }
    blocks(&format!("because no picture protocol was found ({heard})"))
}

/// Which image was drawn where, so the runtime can wipe leftovers of a graphics image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawn {
    pub path: PathBuf,
    pub area: Rect,
}

type Key = (PathBuf, usize, Size);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockCell {
    ch: char,
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

enum Encoded {
    Graphics(Protocol),
    Blocks { cells: Vec<BlockCell>, size: Size },
}

impl Encoded {
    fn size(&self) -> Size {
        match self {
            Encoded::Graphics(protocol) => protocol.size(),
            Encoded::Blocks { size, .. } => *size,
        }
    }
}

pub struct Painter {
    method: Method,
    picker: Option<Picker>,
    font: FontSize,
    reason: String,
    cache: RefCell<Option<(Key, Encoded)>>,
    drawn: RefCell<Option<Drawn>>,
}

impl Painter {
    /// `answers` are what the terminal said when asked at startup, if it was asked.
    pub fn new(mode: Mode, answers: Option<&Answers>) -> Painter {
        let (method, reason) = choose(mode, answers, |name| std::env::var(name).ok());
        let font = answers
            .and_then(|a| a.cell)
            .map(|(w, h)| FontSize::new(w, h))
            .unwrap_or_else(cell_size);
        Painter::build(method, font, reason)
    }

    /// Quadrant blocks, without asking the terminal anything. For tests and pipes.
    pub fn blocks() -> Painter {
        Painter::build(
            Method::Blocks(BlockKind::Quadrants),
            FontSize::new(10, 20),
            "quadrant blocks".into(),
        )
    }

    pub fn halfblocks() -> Painter {
        Painter::build(
            Method::Blocks(BlockKind::Halves),
            FontSize::new(10, 20),
            "half blocks".into(),
        )
    }

    pub fn off() -> Painter {
        Painter::build(Method::Off, FontSize::new(10, 20), "off".into())
    }

    fn build(method: Method, font: FontSize, reason: String) -> Painter {
        let picker = match method {
            Method::Graphics(protocol) => {
                // Deprecated in favour of the library's own terminal query, which leaves a reader
                // thread behind when the terminal is silent. This program asks by itself instead.
                #[allow(deprecated)]
                let mut picker = Picker::from_fontsize(font);
                picker.set_protocol_type(protocol);
                Some(picker)
            }
            _ => None,
        };
        Painter {
            method,
            picker,
            font,
            reason,
            cache: RefCell::new(None),
            drawn: RefCell::new(None),
        }
    }

    pub fn enabled(&self) -> bool {
        self.method != Method::Off
    }

    /// How pictures are drawn and why, in a sentence for the footer.
    pub fn describe(&self) -> String {
        format!("pictures: {}", self.reason)
    }

    /// Whether images are drawn with escape sequences that text drawn over them may not erase.
    pub fn leaves_ghosts(&self) -> bool {
        matches!(
            self.method,
            Method::Graphics(ProtocolType::Sixel | ProtocolType::Iterm2)
        )
    }

    /// Cells the image takes when fitted into `available`, keeping its shape.
    /// Small pictures are enlarged at most twice, since every enlargement only makes bigger blocks.
    pub fn fitted_size(&self, image: &ImageData, available: Size) -> Option<Size> {
        if self.method == Method::Off || available.width == 0 || available.height == 0 {
            return None;
        }
        let (w, h) = (f64::from(image.0.width()), f64::from(image.0.height()));
        let (fw, fh) = (f64::from(self.font.width), f64::from(self.font.height));
        let scale = (f64::from(available.width) * fw / w)
            .min(f64::from(available.height) * fh / h)
            .min(2.0);
        let cols = ((w * scale / fw).round() as u16).clamp(1, available.width);
        let rows = ((h * scale / fh).round() as u16).clamp(1, available.height);
        Some(Size::new(cols, rows))
    }

    /// Draws the image at the top left of `area`, encoding it again only when its size changed.
    pub fn draw(&self, path: &std::path::Path, image: &ImageData, area: Rect, buf: &mut Buffer) {
        let Some(size) = self.fitted_size(image, area.as_size()) else {
            return;
        };
        let key: Key = (path.to_path_buf(), Arc::as_ptr(&image.0) as usize, size);
        let mut cache = self.cache.borrow_mut();
        if cache.as_ref().is_none_or(|(k, _)| *k != key) {
            *cache = self.encode(image, size).map(|encoded| (key, encoded));
        }
        let Some((_, encoded)) = cache.as_ref() else {
            return;
        };
        let placed = Rect {
            width: encoded.size().width.min(area.width),
            height: encoded.size().height.min(area.height),
            ..area
        };
        match encoded {
            Encoded::Graphics(protocol) => Image::new(protocol).render(placed, buf),
            Encoded::Blocks { cells, size } => {
                for (i, cell) in cells.iter().enumerate() {
                    let (x, y) = (i as u16 % size.width, i as u16 / size.width);
                    if x < placed.width && y < placed.height {
                        buf[(placed.x + x, placed.y + y)]
                            .set_char(cell.ch)
                            .set_fg(Color::Rgb(cell.fg.0, cell.fg.1, cell.fg.2))
                            .set_bg(Color::Rgb(cell.bg.0, cell.bg.1, cell.bg.2));
                    }
                }
            }
        }
        *self.drawn.borrow_mut() = Some(Drawn {
            path: path.to_path_buf(),
            area: placed,
        });
    }

    fn encode(&self, image: &ImageData, size: Size) -> Option<Encoded> {
        match self.method {
            Method::Off => None,
            Method::Graphics(_) => {
                let picker = self.picker.as_ref()?;
                let smooth = Resize::Scale(Some(FilterType::CatmullRom));
                picker
                    .new_protocol((*image.0).clone(), size, smooth)
                    .ok()
                    .map(Encoded::Graphics)
            }
            Method::Blocks(kind) => Some(Encoded::Blocks {
                cells: encode_blocks(&image.0, size, kind),
                size,
            }),
        }
    }

    /// What the last frame showed, cleared for the next one.
    pub fn take_drawn(&self) -> Option<Drawn> {
        self.drawn.borrow_mut().take()
    }
}

/// Quadrant characters indexed by which quarters use the foreground colour:
/// 1 top left, 2 top right, 4 bottom left, 8 bottom right.
const QUADRANTS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

type Rgb = (u8, u8, u8);

fn mean(pixels: &[Rgb]) -> Rgb {
    let n = pixels.len().max(1) as u32;
    let sum = pixels.iter().fold((0u32, 0u32, 0u32), |s, p| {
        (
            s.0 + u32::from(p.0),
            s.1 + u32::from(p.1),
            s.2 + u32::from(p.2),
        )
    });
    ((sum.0 / n) as u8, (sum.1 / n) as u8, (sum.2 / n) as u8)
}

fn error(pixels: &[Rgb], color: Rgb) -> u32 {
    pixels
        .iter()
        .map(|p| {
            let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2) as u32;
            d(p.0, color.0) + d(p.1, color.1) + d(p.2, color.2)
        })
        .sum()
}

/// Splits the four pixels of a cell into the two colour groups that match them best, the way
/// chafa picks block symbols. Returns the character and its foreground and background colours.
fn best_quadrant(pixels: [Rgb; 4]) -> BlockCell {
    let mut best = BlockCell {
        ch: ' ',
        fg: mean(&pixels),
        bg: mean(&pixels),
    };
    let mut best_error = error(&pixels, best.bg);
    // A mask and its complement split the pixels the same way, so the masks up to 7 cover every split.
    for mask in 1..8usize {
        let (fg, bg): (Vec<Rgb>, Vec<Rgb>) = (0..4).map(|i| (mask >> i & 1 == 1, pixels[i])).fold(
            (Vec::new(), Vec::new()),
            |(mut f, mut b), (on, p)| {
                if on {
                    f.push(p)
                } else {
                    b.push(p)
                }
                (f, b)
            },
        );
        let (fg_color, bg_color) = (mean(&fg), mean(&bg));
        let total = error(&fg, fg_color) + error(&bg, bg_color);
        if total < best_error {
            best_error = total;
            best = BlockCell {
                ch: QUADRANTS[mask],
                fg: fg_color,
                bg: bg_color,
            };
        }
    }
    best
}

/// Draws a picture with block characters: resized with a smoothing filter to the pixels the cells
/// can show, transparent parts laid over the page background.
fn encode_blocks(image: &DynamicImage, size: Size, kind: BlockKind) -> Vec<BlockCell> {
    let (cols, rows) = (u32::from(size.width), u32::from(size.height));
    let across = if kind == BlockKind::Quadrants { 2 } else { 1 };
    let pixels = image
        .resize_exact(cols * across, rows * 2, FilterType::CatmullRom)
        .to_rgba8();
    let background = crate::theme::BG;
    let at = |x: u32, y: u32| -> Rgb {
        let p = pixels.get_pixel(x, y).0;
        let a = u32::from(p[3]);
        let over = |c: u8, b: u8| ((u32::from(c) * a + u32::from(b) * (255 - a)) / 255) as u8;
        (
            over(p[0], background.0),
            over(p[1], background.1),
            over(p[2], background.2),
        )
    };
    let mut cells = Vec::with_capacity((cols * rows) as usize);
    for y in 0..rows {
        for x in 0..cols {
            let cell = match kind {
                BlockKind::Quadrants => best_quadrant([
                    at(2 * x, 2 * y),
                    at(2 * x + 1, 2 * y),
                    at(2 * x, 2 * y + 1),
                    at(2 * x + 1, 2 * y + 1),
                ]),
                BlockKind::Halves => {
                    let (top, bottom) = (at(x, 2 * y), at(x, 2 * y + 1));
                    BlockCell {
                        ch: if top == bottom { ' ' } else { '▀' },
                        fg: top,
                        bg: bottom,
                    }
                }
            };
            cells.push(cell);
        }
    }
    cells
}

/// Pixel size of one terminal cell, from the size the terminal reports for its window.
fn cell_size() -> FontSize {
    match ratatui::crossterm::terminal::window_size() {
        Ok(w) if w.width > 0 && w.height > 0 && w.columns > 0 && w.rows > 0 => {
            FontSize::new((w.width / w.columns).max(1), (w.height / w.rows).max(1))
        }
        _ => FontSize::new(10, 20),
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;

    fn picture(w: u32, h: u32) -> ImageData {
        let mut img = RgbaImage::new(w, h);
        for (x, _, px) in img.enumerate_pixels_mut() {
            *px = if x < w / 2 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            };
        }
        ImageData(Arc::new(DynamicImage::ImageRgba8(img)))
    }

    #[test]
    fn modes_parse_from_config_words() {
        assert_eq!(Mode::parse("auto"), Some(Mode::Auto));
        assert_eq!(Mode::parse("sixel"), Some(Mode::Sixel));
        assert_eq!(Mode::parse("off"), Some(Mode::Off));
        assert_eq!(Mode::parse("svga"), None);
    }

    #[test]
    fn halfblocks_draw_the_image_colours_into_cells() {
        let painter = Painter::halfblocks();
        let image = picture(40, 40);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 20));
        painter.draw(
            std::path::Path::new("/p.png"),
            &image,
            Rect::new(2, 1, 20, 10),
            &mut buf,
        );
        let drawn = painter.take_drawn().expect("something was drawn");
        assert_eq!((drawn.area.x, drawn.area.y), (2, 1));
        assert!(drawn.area.width <= 20 && drawn.area.height <= 10);
        let left = &buf[(drawn.area.x, drawn.area.y)];
        let right = &buf[(drawn.area.right() - 1, drawn.area.y)];
        assert_ne!(left.fg, right.fg, "red half and blue half differ");
        assert_eq!(buf[(0, 0)].symbol(), " ", "nothing outside the area");
        assert!(painter.take_drawn().is_none(), "the record is per frame");
    }

    #[test]
    fn a_fine_checkerboard_shrinks_to_grey_instead_of_random_black_and_white() {
        let board = image::RgbImage::from_fn(1000, 1000, |x, y| {
            if (x + y) % 2 == 0 {
                image::Rgb([0, 0, 0])
            } else {
                image::Rgb([255, 255, 255])
            }
        });
        let image = ImageData(Arc::new(DynamicImage::ImageRgb8(board)));
        let painter = Painter::halfblocks();
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        painter.draw(
            std::path::Path::new("/board.png"),
            &image,
            buf.area,
            &mut buf,
        );
        let area = painter.take_drawn().unwrap().area;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                for color in [buf[(x, y)].fg, buf[(x, y)].bg] {
                    let ratatui::style::Color::Rgb(r, _, _) = color else {
                        panic!("expected rgb, got {color:?}")
                    };
                    assert!((60..=195).contains(&r), "cell {x},{y} is {r}, not grey");
                }
            }
        }
    }

    #[test]
    fn a_picture_fills_the_room_keeping_its_shape_and_icons_grow_at_most_twice() {
        let painter = Painter::halfblocks();
        let big = painter
            .fitted_size(&picture(2000, 1000), Size::new(40, 40))
            .unwrap();
        assert_eq!(big.width, 40);
        assert!(big.height < 40, "keeps the 2:1 shape: {big:?}");
        let icon = painter
            .fitted_size(&picture(40, 40), Size::new(100, 100))
            .unwrap();
        assert!(
            icon.width > 4,
            "a 40 px icon is 4 cells wide at 10 px a cell, so it grows: {icon:?}"
        );
        assert!(icon.width <= 8, "but no more than twice: {icon:?}");
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn answers(kitty: bool, sixel: bool, name: Option<&str>) -> Answers {
        Answers {
            kitty,
            sixel,
            name: name.map(String::from),
            cell: None,
            complete: true,
        }
    }

    fn method(
        mode: Mode,
        a: Option<&Answers>,
        vars: &'static [(&'static str, &'static str)],
    ) -> Method {
        choose(mode, a, env(vars)).0
    }

    #[test]
    fn the_terminals_answers_pick_the_protocol() {
        use ProtocolType::*;
        let xterm = &[("TERM", "xterm-256color")];
        assert_eq!(
            method(
                Mode::Auto,
                Some(&answers(true, false, Some("kitty(0.35)"))),
                xterm
            ),
            Method::Graphics(Kitty)
        );
        assert_eq!(
            method(
                Mode::Auto,
                Some(&answers(false, true, Some("XTerm(390)"))),
                xterm
            ),
            Method::Graphics(Sixel)
        );
        assert_eq!(
            method(
                Mode::Auto,
                Some(&answers(true, true, Some("WezTerm 2024"))),
                xterm
            ),
            Method::Graphics(Iterm2),
            "WezTerm lacks kitty placeholders"
        );
        assert_eq!(
            method(
                Mode::Auto,
                Some(&answers(true, true, Some("Konsole 23.08"))),
                xterm
            ),
            Method::Graphics(Sixel),
            "so does Konsole"
        );
        assert_eq!(
            method(
                Mode::Auto,
                Some(&answers(false, false, Some("VTE(7600)"))),
                xterm
            ),
            Method::Blocks(BlockKind::Quadrants)
        );
    }

    #[test]
    fn without_answers_the_environment_decides_and_blocks_are_the_fallback() {
        use ProtocolType::*;
        assert_eq!(
            method(Mode::Auto, None, &[("TERM", "xterm-kitty")]),
            Method::Graphics(Kitty)
        );
        assert_eq!(
            method(Mode::Auto, None, &[("TERM", "foot")]),
            Method::Graphics(Sixel)
        );
        assert_eq!(
            method(Mode::Auto, None, &[("TERM_PROGRAM", "iTerm.app")]),
            Method::Graphics(Iterm2)
        );
        assert_eq!(
            method(Mode::Auto, None, &[("TERM", "xterm-256color")]),
            Method::Blocks(BlockKind::Quadrants)
        );
    }

    #[test]
    fn tmux_passes_graphics_on_only_when_the_library_can_wrap_them() {
        use ProtocolType::*;
        let kitty = answers(true, false, Some("kitty(0.35)"));
        assert_eq!(
            method(
                Mode::Auto,
                Some(&kitty),
                &[("TMUX", "/tmp/t"), ("TERM", "tmux-256color")]
            ),
            Method::Graphics(Kitty)
        );
        assert_eq!(
            method(
                Mode::Auto,
                Some(&kitty),
                &[("TMUX", "/tmp/t"), ("TERM", "screen-256color")]
            ),
            Method::Blocks(BlockKind::Quadrants)
        );
    }

    #[test]
    fn inside_tmux_its_own_sixel_answer_is_not_trusted() {
        let tmux_says_sixel = answers(false, true, None);
        assert_eq!(
            method(
                Mode::Auto,
                Some(&tmux_says_sixel),
                &[("TMUX", "/tmp/t"), ("TERM", "tmux-256color")]
            ),
            Method::Blocks(BlockKind::Quadrants)
        );
    }

    #[test]
    fn the_config_overrides_detection_and_the_reason_says_why() {
        let (m, why) = choose(Mode::Sixel, None, env(&[("TERM", "xterm")]));
        assert_eq!(m, Method::Graphics(ProtocolType::Sixel));
        assert!(why.contains("set in the config"), "{why}");
        let (_, why) = choose(Mode::Auto, None, env(&[("TERM", "xterm")]));
        assert!(why.contains("did not answer"), "{why}");
        let (_, why) = choose(
            Mode::Auto,
            Some(&answers(false, true, Some("foot(1.16)"))),
            env(&[]),
        );
        assert!(why.contains("foot(1.16)"), "{why}");
    }

    #[test]
    fn a_cell_splits_into_the_two_colours_that_fit_its_quarters_best() {
        let (r, b) = ((250, 0, 0), (0, 0, 250));
        let cell = best_quadrant([r, b, r, b]);
        assert!(matches!(cell.ch, '▌' | '▐'), "a vertical split: {cell:?}");
        let cell = best_quadrant([r, r, b, b]);
        assert!(matches!(cell.ch, '▀' | '▄'), "a horizontal split: {cell:?}");
        let cell = best_quadrant([r, b, b, b]);
        assert_eq!(cell.ch, '▘');
        assert_eq!((cell.fg, cell.bg), (r, b));
        let flat = best_quadrant([r, r, r, r]);
        assert_eq!((flat.ch, flat.bg), (' ', r));
    }

    #[test]
    fn quadrants_show_twice_the_detail_of_half_blocks_across() {
        let stripes = image::RgbImage::from_fn(40, 40, |x, _| {
            if (x / 10) % 2 == 0 {
                image::Rgb([255, 255, 255])
            } else {
                image::Rgb([0, 0, 0])
            }
        });
        let image = DynamicImage::ImageRgb8(stripes);
        let quads = encode_blocks(&image, Size::new(2, 1), BlockKind::Quadrants);
        assert!(
            quads.iter().all(|c| matches!(c.ch, '▌' | '▐')),
            "each cell holds a white and a black stripe: {quads:?}"
        );
        let halves = encode_blocks(&image, Size::new(2, 1), BlockKind::Halves);
        assert!(
            halves.iter().all(|c| c.ch == ' '),
            "half blocks cannot split a cell sideways"
        );
    }

    #[test]
    fn transparent_pixels_take_the_page_background() {
        let clear = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 4, Rgba([255, 255, 255, 0])));
        let cells = encode_blocks(&clear, Size::new(1, 1), BlockKind::Quadrants);
        assert_eq!(cells[0].bg, crate::theme::BG);
    }

    #[test]
    fn switched_off_draws_nothing() {
        let painter = Painter::off();
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        painter.draw(
            std::path::Path::new("/p.png"),
            &picture(8, 8),
            buf.area,
            &mut buf,
        );
        assert!(painter.take_drawn().is_none());
        assert!(!painter.enabled());
        assert_eq!(painter.describe(), "pictures: off");
    }

    #[test]
    fn only_sixel_and_iterm2_need_a_repaint_to_clear() {
        assert!(!Painter::blocks().leaves_ghosts());
        assert!(!Painter::off().leaves_ghosts());
        let sixel = Painter::build(
            Method::Graphics(ProtocolType::Sixel),
            FontSize::new(10, 20),
            String::new(),
        );
        assert!(sixel.leaves_ghosts());
    }
}
