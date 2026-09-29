//! Draws decoded images in the preview column with the best graphics protocol the terminal
//! offers: kitty, sixel, iTerm2, or coloured half blocks everywhere else.

use std::{cell::RefCell, path::PathBuf, sync::Arc};

use image::DynamicImage;
use ratatui::{
    buffer::Buffer,
    layout::{Rect, Size},
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
            "halfblocks" => Mode::Halfblocks,
            "off" => Mode::Off,
            _ => return None,
        })
    }
}

/// Which image was drawn where, so the runtime can wipe leftovers of a graphics image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawn {
    pub path: PathBuf,
    pub area: Rect,
}

type Key = (PathBuf, usize, Size);

pub struct Painter {
    picker: Option<Picker>,
    cache: RefCell<Option<(Key, Protocol)>>,
    drawn: RefCell<Option<Drawn>>,
}

impl Painter {
    /// Picks the protocol from the environment, without asking the terminal anything. A query would
    /// read stdin, and a terminal that never answers would make it stall and swallow keystrokes.
    pub fn new(mode: Mode) -> Painter {
        let protocol = match mode {
            Mode::Off => return Painter::off(),
            Mode::Auto => detect_protocol(|name| std::env::var(name).ok()),
            Mode::Kitty => ProtocolType::Kitty,
            Mode::Sixel => ProtocolType::Sixel,
            Mode::Iterm2 => ProtocolType::Iterm2,
            Mode::Halfblocks => ProtocolType::Halfblocks,
        };
        // Deprecated in favour of querying the terminal, which is exactly what must not happen here (see above).
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(cell_size());
        picker.set_protocol_type(protocol);
        Painter::with_picker(Some(picker))
    }

    /// Half blocks without asking the terminal anything. For tests and pipes.
    pub fn halfblocks() -> Painter {
        Painter::with_picker(Some(Picker::halfblocks()))
    }

    pub fn off() -> Painter {
        Painter::with_picker(None)
    }

    fn with_picker(picker: Option<Picker>) -> Painter {
        Painter {
            picker,
            cache: RefCell::new(None),
            drawn: RefCell::new(None),
        }
    }

    pub fn enabled(&self) -> bool {
        self.picker.is_some()
    }

    /// Whether images are drawn with escape sequences that text drawn over them may not erase.
    pub fn leaves_ghosts(&self) -> bool {
        self.picker.as_ref().is_some_and(|p| {
            matches!(
                p.protocol_type(),
                ProtocolType::Sixel | ProtocolType::Iterm2
            )
        })
    }

    pub fn protocol_name(&self) -> &'static str {
        match self.picker.as_ref().map(Picker::protocol_type) {
            None => "off",
            Some(ProtocolType::Kitty) => "kitty",
            Some(ProtocolType::Sixel) => "sixel",
            Some(ProtocolType::Iterm2) => "iterm2",
            Some(ProtocolType::Halfblocks) => "halfblocks",
        }
    }

    /// Cells the image takes when fitted into `available`, keeping its shape.
    /// Small pictures are enlarged at most twice, since every enlargement only makes bigger blocks.
    pub fn fitted_size(&self, image: &ImageData, available: Size) -> Option<Size> {
        let picker = self.picker.as_ref()?;
        let natural = Resize::natural_size(&image.0, picker.font_size());
        let bound = Size::new(
            available.width.min(natural.width.saturating_mul(2)),
            available.height.min(natural.height.saturating_mul(2)),
        );
        let size = smooth().size_for(&image.0, picker.font_size(), bound);
        (size.width > 0 && size.height > 0).then_some(size)
    }

    /// Draws the image at the top left of `area`, encoding it again only when its size changed.
    pub fn draw(&self, path: &std::path::Path, image: &ImageData, area: Rect, buf: &mut Buffer) {
        let Some(picker) = &self.picker else { return };
        let Some(size) = self.fitted_size(image, area.as_size()) else {
            return;
        };
        let key: Key = (path.to_path_buf(), Arc::as_ptr(&image.0) as usize, size);
        let mut cache = self.cache.borrow_mut();
        if cache.as_ref().is_none_or(|(k, _)| *k != key) {
            let encoded = picker.new_protocol((*image.0).clone(), size, smooth());
            *cache = encoded.ok().map(|protocol| (key, protocol));
        }
        let Some((_, protocol)) = cache.as_ref() else {
            return;
        };
        let placed = Rect {
            width: protocol.size().width.min(area.width),
            height: protocol.size().height.min(area.height),
            ..area
        };
        Image::new(protocol).render(placed, buf);
        *self.drawn.borrow_mut() = Some(Drawn {
            path: path.to_path_buf(),
            area: placed,
        });
    }

    /// What the last frame showed, cleared for the next one.
    pub fn take_drawn(&self) -> Option<Drawn> {
        self.drawn.borrow_mut().take()
    }
}

/// Scaling that averages neighbouring pixels. The library's default keeps one pixel out of many,
/// which turns photos into jagged noise.
fn smooth() -> Resize {
    Resize::Scale(Some(image::imageops::FilterType::CatmullRom))
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

/// The graphics protocol the terminal most likely speaks, from the variables terminals set.
/// Inside tmux or screen pictures use half blocks, because passing graphics through them is unreliable.
pub fn detect_protocol(var: impl Fn(&str) -> Option<String>) -> ProtocolType {
    let get = |name: &str| var(name).unwrap_or_default();
    let term = get("TERM");
    let program = get("TERM_PROGRAM");
    if var("TMUX").is_some() || term.starts_with("tmux") || term.starts_with("screen") {
        return ProtocolType::Halfblocks;
    }
    if term == "xterm-kitty"
        || term == "xterm-ghostty"
        || program == "ghostty"
        || var("KITTY_WINDOW_ID").is_some()
    {
        return ProtocolType::Kitty;
    }
    if program.contains("WezTerm")
        || program.contains("iTerm")
        || program.contains("mintty")
        || program.contains("vscode")
    {
        return ProtocolType::Iterm2;
    }
    if term.starts_with("foot")
        || term.starts_with("mlterm")
        || term.starts_with("yaft")
        || program.contains("contour")
        || var("KONSOLE_VERSION").is_some()
    {
        return ProtocolType::Sixel;
    }
    ProtocolType::Halfblocks
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

    #[test]
    fn the_protocol_is_chosen_from_the_environment() {
        use ProtocolType::*;
        assert_eq!(detect_protocol(env(&[("TERM", "xterm-kitty")])), Kitty);
        assert_eq!(detect_protocol(env(&[("TERM_PROGRAM", "ghostty")])), Kitty);
        assert_eq!(detect_protocol(env(&[("TERM_PROGRAM", "WezTerm")])), Iterm2);
        assert_eq!(detect_protocol(env(&[("TERM", "foot")])), Sixel);
        assert_eq!(
            detect_protocol(env(&[
                ("TERM", "xterm-256color"),
                ("KONSOLE_VERSION", "230804")
            ])),
            Sixel
        );
        assert_eq!(
            detect_protocol(env(&[("TERM", "xterm-256color")])),
            Halfblocks
        );
        assert_eq!(
            detect_protocol(env(&[("TERM", "tmux-256color"), ("KITTY_WINDOW_ID", "1")])),
            Halfblocks,
            "inside tmux a stale kitty variable is ignored"
        );
        assert_eq!(
            detect_protocol(env(&[("TMUX", "/tmp/x"), ("TERM", "xterm-kitty")])),
            Halfblocks
        );
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
        assert_eq!(painter.protocol_name(), "off");
    }

    #[test]
    fn only_sixel_and_iterm2_need_a_repaint_to_clear() {
        assert!(!Painter::halfblocks().leaves_ghosts());
        assert!(!Painter::off().leaves_ghosts());
    }
}
