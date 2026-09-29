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
