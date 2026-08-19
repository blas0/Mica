//! Synthesised box-drawing and block glyphs.
//!
//! **Box-drawing characters are never taken from the font.** A font rasterises
//! U+2500–257F to fit its own cell metrics, which are not Mica's cell metrics;
//! the result is that adjacent box glyphs miss each other by a fraction of a
//! pixel and every table, every `tree`, and every TUI border shows hairline
//! seams. Drawing them ourselves against the exact cell rectangle makes the
//! seams structurally impossible rather than merely unlikely.
//!
//! Everything here is pure: it takes a cell size and returns an 8-bit coverage
//! bitmap. No CoreText, no Metal, no font. That is what makes it testable —
//! and the seam property is asserted in the tests below by tiling two glyphs
//! and checking the join.
//!
//! ## Where the table comes from
//!
//! The 128 entries of [`BOX_TABLE`] were generated from the Unicode character
//! names in `UnicodeData.txt`, not typed by hand. A name like
//! `BOX DRAWINGS LEFT UP HEAVY AND RIGHT DOWN LIGHT` fully specifies the glyph
//! — one stroke weight per direction — so deriving the table from the names is
//! both less work and less error-prone than transcribing 128 pictures.

/// Coverage bitmap: one byte of alpha per pixel, row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

impl Coverage {
    fn new(width: u16, height: u16) -> Coverage {
        Coverage { width, height, data: vec![0u8; width as usize * height as usize] }
    }

    pub fn get(&self, x: u16, y: u16) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data[y as usize * self.width as usize + x as usize]
    }

    fn put(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = y as usize * self.width as usize + x as usize;
        // Coverage saturates rather than adds: two strokes crossing must not
        // produce a brighter pixel than either alone.
        self.data[i] = self.data[i].max(value);
    }

    fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, 255);
            }
        }
    }

    pub fn is_blank(&self) -> bool {
        self.data.iter().all(|&v| v == 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Diagonal {
    UpRightToLowLeft,
    UpLeftToLowRight,
    Cross,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Line,
    Arc,
    Diagonal(Diagonal),
}

#[derive(Debug, Clone, Copy)]
struct BoxSpec {
    shape: Shape,
    /// `[up, down, left, right]`, each 0 none / 1 light / 2 heavy / 3 double.
    weights: [u8; 4],
    /// 0 = solid, else the number of dashes across the cell.
    dashes: u8,
}

/// Generated from `UnicodeData.txt` character names — see the module docs.
/// `weights` is `[up, down, left, right]`: 0 none, 1 light, 2 heavy, 3 double.
const BOX_TABLE: [BoxSpec; 128] = [
    BoxSpec { shape: Shape::Line, weights: [0, 0, 1, 1], dashes: 0 }, // U+2500 LIGHT HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 2, 2], dashes: 0 }, // U+2501 HEAVY HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 0], dashes: 0 }, // U+2502 LIGHT VERTICAL
    BoxSpec { shape: Shape::Line, weights: [2, 2, 0, 0], dashes: 0 }, // U+2503 HEAVY VERTICAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 1, 1], dashes: 3 }, // U+2504 LIGHT HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 2, 2], dashes: 3 }, // U+2505 HEAVY HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 0], dashes: 3 }, // U+2506 LIGHT VERTICAL
    BoxSpec { shape: Shape::Line, weights: [2, 2, 0, 0], dashes: 3 }, // U+2507 HEAVY VERTICAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 1, 1], dashes: 4 }, // U+2508 LIGHT HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 2, 2], dashes: 4 }, // U+2509 HEAVY HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 0], dashes: 4 }, // U+250A LIGHT VERTICAL
    BoxSpec { shape: Shape::Line, weights: [2, 2, 0, 0], dashes: 4 }, // U+250B HEAVY VERTICAL
    BoxSpec { shape: Shape::Line, weights: [0, 1, 0, 1], dashes: 0 }, // U+250C LIGHT DOWN AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 0, 2], dashes: 0 }, // U+250D DOWN LIGHT AND RIGHT HEAVY
    BoxSpec { shape: Shape::Line, weights: [0, 2, 0, 1], dashes: 0 }, // U+250E DOWN HEAVY AND RIGHT LIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 2, 0, 2], dashes: 0 }, // U+250F HEAVY DOWN AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 1, 0], dashes: 0 }, // U+2510 LIGHT DOWN AND LEFT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 2, 0], dashes: 0 }, // U+2511 DOWN LIGHT AND LEFT HEAVY
    BoxSpec { shape: Shape::Line, weights: [0, 2, 1, 0], dashes: 0 }, // U+2512 DOWN HEAVY AND LEFT LIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 2, 2, 0], dashes: 0 }, // U+2513 HEAVY DOWN AND LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 0, 1], dashes: 0 }, // U+2514 LIGHT UP AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 0, 2], dashes: 0 }, // U+2515 UP LIGHT AND RIGHT HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 0, 0, 1], dashes: 0 }, // U+2516 UP HEAVY AND RIGHT LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 0, 0, 2], dashes: 0 }, // U+2517 HEAVY UP AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 1, 0], dashes: 0 }, // U+2518 LIGHT UP AND LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 2, 0], dashes: 0 }, // U+2519 UP LIGHT AND LEFT HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 0, 1, 0], dashes: 0 }, // U+251A UP HEAVY AND LEFT LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 0, 2, 0], dashes: 0 }, // U+251B HEAVY UP AND LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 1], dashes: 0 }, // U+251C LIGHT VERTICAL AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 2], dashes: 0 }, // U+251D VERTICAL LIGHT AND RIGHT HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 1, 0, 1], dashes: 0 }, // U+251E UP HEAVY AND RIGHT DOWN LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 2, 0, 1], dashes: 0 }, // U+251F DOWN HEAVY AND RIGHT UP LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 2, 0, 1], dashes: 0 }, // U+2520 VERTICAL HEAVY AND RIGHT LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 1, 0, 2], dashes: 0 }, // U+2521 DOWN LIGHT AND RIGHT UP HEAVY
    BoxSpec { shape: Shape::Line, weights: [1, 2, 0, 2], dashes: 0 }, // U+2522 UP LIGHT AND RIGHT DOWN HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 2, 0, 2], dashes: 0 }, // U+2523 HEAVY VERTICAL AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 1, 0], dashes: 0 }, // U+2524 LIGHT VERTICAL AND LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 2, 0], dashes: 0 }, // U+2525 VERTICAL LIGHT AND LEFT HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 1, 1, 0], dashes: 0 }, // U+2526 UP HEAVY AND LEFT DOWN LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 2, 1, 0], dashes: 0 }, // U+2527 DOWN HEAVY AND LEFT UP LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 2, 1, 0], dashes: 0 }, // U+2528 VERTICAL HEAVY AND LEFT LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 1, 2, 0], dashes: 0 }, // U+2529 DOWN LIGHT AND LEFT UP HEAVY
    BoxSpec { shape: Shape::Line, weights: [1, 2, 2, 0], dashes: 0 }, // U+252A UP LIGHT AND LEFT DOWN HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 2, 2, 0], dashes: 0 }, // U+252B HEAVY VERTICAL AND LEFT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 1, 1], dashes: 0 }, // U+252C LIGHT DOWN AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [0, 1, 2, 1], dashes: 0 }, // U+252D LEFT HEAVY AND RIGHT DOWN LIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 1, 2], dashes: 0 }, // U+252E RIGHT HEAVY AND LEFT DOWN LIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 2, 2], dashes: 0 }, // U+252F DOWN LIGHT AND HORIZONTAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [0, 2, 1, 1], dashes: 0 }, // U+2530 DOWN HEAVY AND HORIZONTAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 2, 2, 1], dashes: 0 }, // U+2531 RIGHT LIGHT AND LEFT DOWN HEAVY
    BoxSpec { shape: Shape::Line, weights: [0, 2, 1, 2], dashes: 0 }, // U+2532 LEFT LIGHT AND RIGHT DOWN HEAVY
    BoxSpec { shape: Shape::Line, weights: [0, 2, 2, 2], dashes: 0 }, // U+2533 HEAVY DOWN AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 0, 1, 1], dashes: 0 }, // U+2534 LIGHT UP AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 0, 2, 1], dashes: 0 }, // U+2535 LEFT HEAVY AND RIGHT UP LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 1, 2], dashes: 0 }, // U+2536 RIGHT HEAVY AND LEFT UP LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 2, 2], dashes: 0 }, // U+2537 UP LIGHT AND HORIZONTAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 0, 1, 1], dashes: 0 }, // U+2538 UP HEAVY AND HORIZONTAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 0, 2, 1], dashes: 0 }, // U+2539 RIGHT LIGHT AND LEFT UP HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 0, 1, 2], dashes: 0 }, // U+253A LEFT LIGHT AND RIGHT UP HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 0, 2, 2], dashes: 0 }, // U+253B HEAVY UP AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 1, 1], dashes: 0 }, // U+253C LIGHT VERTICAL AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 2, 1], dashes: 0 }, // U+253D LEFT HEAVY AND RIGHT VERTICAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 1, 2], dashes: 0 }, // U+253E RIGHT HEAVY AND LEFT VERTICAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 2, 2], dashes: 0 }, // U+253F VERTICAL LIGHT AND HORIZONTAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 1, 1, 1], dashes: 0 }, // U+2540 UP HEAVY AND DOWN HORIZONTAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 2, 1, 1], dashes: 0 }, // U+2541 DOWN HEAVY AND UP HORIZONTAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 2, 1, 1], dashes: 0 }, // U+2542 VERTICAL HEAVY AND HORIZONTAL LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 1, 2, 1], dashes: 0 }, // U+2543 LEFT UP HEAVY AND RIGHT DOWN LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 1, 1, 2], dashes: 0 }, // U+2544 RIGHT UP HEAVY AND LEFT DOWN LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 2, 2, 1], dashes: 0 }, // U+2545 LEFT DOWN HEAVY AND RIGHT UP LIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 2, 1, 2], dashes: 0 }, // U+2546 RIGHT DOWN HEAVY AND LEFT UP LIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 1, 2, 2], dashes: 0 }, // U+2547 DOWN LIGHT AND UP HORIZONTAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [1, 2, 2, 2], dashes: 0 }, // U+2548 UP LIGHT AND DOWN HORIZONTAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 2, 2, 1], dashes: 0 }, // U+2549 RIGHT LIGHT AND LEFT VERTICAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 2, 1, 2], dashes: 0 }, // U+254A LEFT LIGHT AND RIGHT VERTICAL HEAVY
    BoxSpec { shape: Shape::Line, weights: [2, 2, 2, 2], dashes: 0 }, // U+254B HEAVY VERTICAL AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 1, 1], dashes: 2 }, // U+254C LIGHT HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 2, 2], dashes: 2 }, // U+254D HEAVY HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 0], dashes: 2 }, // U+254E LIGHT VERTICAL
    BoxSpec { shape: Shape::Line, weights: [2, 2, 0, 0], dashes: 2 }, // U+254F HEAVY VERTICAL
    BoxSpec { shape: Shape::Line, weights: [0, 0, 3, 3], dashes: 0 }, // U+2550 DOUBLE HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [3, 3, 0, 0], dashes: 0 }, // U+2551 DOUBLE VERTICAL
    BoxSpec { shape: Shape::Line, weights: [0, 1, 0, 3], dashes: 0 }, // U+2552 DOWN SINGLE AND RIGHT DOUBLE
    BoxSpec { shape: Shape::Line, weights: [0, 3, 0, 1], dashes: 0 }, // U+2553 DOWN DOUBLE AND RIGHT SINGLE
    BoxSpec { shape: Shape::Line, weights: [0, 3, 0, 3], dashes: 0 }, // U+2554 DOUBLE DOWN AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 3, 0], dashes: 0 }, // U+2555 DOWN SINGLE AND LEFT DOUBLE
    BoxSpec { shape: Shape::Line, weights: [0, 3, 1, 0], dashes: 0 }, // U+2556 DOWN DOUBLE AND LEFT SINGLE
    BoxSpec { shape: Shape::Line, weights: [0, 3, 3, 0], dashes: 0 }, // U+2557 DOUBLE DOWN AND LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 0, 3], dashes: 0 }, // U+2558 UP SINGLE AND RIGHT DOUBLE
    BoxSpec { shape: Shape::Line, weights: [3, 0, 0, 1], dashes: 0 }, // U+2559 UP DOUBLE AND RIGHT SINGLE
    BoxSpec { shape: Shape::Line, weights: [3, 0, 0, 3], dashes: 0 }, // U+255A DOUBLE UP AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 3, 0], dashes: 0 }, // U+255B UP SINGLE AND LEFT DOUBLE
    BoxSpec { shape: Shape::Line, weights: [3, 0, 1, 0], dashes: 0 }, // U+255C UP DOUBLE AND LEFT SINGLE
    BoxSpec { shape: Shape::Line, weights: [3, 0, 3, 0], dashes: 0 }, // U+255D DOUBLE UP AND LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 0, 3], dashes: 0 }, // U+255E VERTICAL SINGLE AND RIGHT DOUBLE
    BoxSpec { shape: Shape::Line, weights: [3, 3, 0, 1], dashes: 0 }, // U+255F VERTICAL DOUBLE AND RIGHT SINGLE
    BoxSpec { shape: Shape::Line, weights: [3, 3, 0, 3], dashes: 0 }, // U+2560 DOUBLE VERTICAL AND RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 1, 3, 0], dashes: 0 }, // U+2561 VERTICAL SINGLE AND LEFT DOUBLE
    BoxSpec { shape: Shape::Line, weights: [3, 3, 1, 0], dashes: 0 }, // U+2562 VERTICAL DOUBLE AND LEFT SINGLE
    BoxSpec { shape: Shape::Line, weights: [3, 3, 3, 0], dashes: 0 }, // U+2563 DOUBLE VERTICAL AND LEFT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 3, 3], dashes: 0 }, // U+2564 DOWN SINGLE AND HORIZONTAL DOUBLE
    BoxSpec { shape: Shape::Line, weights: [0, 3, 1, 1], dashes: 0 }, // U+2565 DOWN DOUBLE AND HORIZONTAL SINGLE
    BoxSpec { shape: Shape::Line, weights: [0, 3, 3, 3], dashes: 0 }, // U+2566 DOUBLE DOWN AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 0, 3, 3], dashes: 0 }, // U+2567 UP SINGLE AND HORIZONTAL DOUBLE
    BoxSpec { shape: Shape::Line, weights: [3, 0, 1, 1], dashes: 0 }, // U+2568 UP DOUBLE AND HORIZONTAL SINGLE
    BoxSpec { shape: Shape::Line, weights: [3, 0, 3, 3], dashes: 0 }, // U+2569 DOUBLE UP AND HORIZONTAL
    BoxSpec { shape: Shape::Line, weights: [1, 1, 3, 3], dashes: 0 }, // U+256A VERTICAL SINGLE AND HORIZONTAL DOUBLE
    BoxSpec { shape: Shape::Line, weights: [3, 3, 1, 1], dashes: 0 }, // U+256B VERTICAL DOUBLE AND HORIZONTAL SINGLE
    BoxSpec { shape: Shape::Line, weights: [3, 3, 3, 3], dashes: 0 }, // U+256C DOUBLE VERTICAL AND HORIZONTAL
    BoxSpec { shape: Shape::Arc, weights: [0, 1, 0, 1], dashes: 0 }, // U+256D LIGHT DOWN AND RIGHT
    BoxSpec { shape: Shape::Arc, weights: [0, 1, 1, 0], dashes: 0 }, // U+256E LIGHT DOWN AND LEFT
    BoxSpec { shape: Shape::Arc, weights: [1, 0, 1, 0], dashes: 0 }, // U+256F LIGHT UP AND LEFT
    BoxSpec { shape: Shape::Arc, weights: [1, 0, 0, 1], dashes: 0 }, // U+2570 LIGHT UP AND RIGHT
    BoxSpec { shape: Shape::Diagonal(Diagonal::UpRightToLowLeft), weights: [0, 0, 0, 0], dashes: 0 }, // U+2571 LIGHT DIAGONAL UPPER RIGHT TO LOWER LEFT
    BoxSpec { shape: Shape::Diagonal(Diagonal::UpLeftToLowRight), weights: [0, 0, 0, 0], dashes: 0 }, // U+2572 LIGHT DIAGONAL UPPER LEFT TO LOWER RIGHT
    BoxSpec { shape: Shape::Diagonal(Diagonal::Cross), weights: [0, 0, 0, 0], dashes: 0 }, // U+2573 LIGHT DIAGONAL CROSS
    BoxSpec { shape: Shape::Line, weights: [0, 0, 1, 0], dashes: 0 }, // U+2574 LIGHT LEFT
    BoxSpec { shape: Shape::Line, weights: [1, 0, 0, 0], dashes: 0 }, // U+2575 LIGHT UP
    BoxSpec { shape: Shape::Line, weights: [0, 0, 0, 1], dashes: 0 }, // U+2576 LIGHT RIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 1, 0, 0], dashes: 0 }, // U+2577 LIGHT DOWN
    BoxSpec { shape: Shape::Line, weights: [0, 0, 2, 0], dashes: 0 }, // U+2578 HEAVY LEFT
    BoxSpec { shape: Shape::Line, weights: [2, 0, 0, 0], dashes: 0 }, // U+2579 HEAVY UP
    BoxSpec { shape: Shape::Line, weights: [0, 0, 0, 2], dashes: 0 }, // U+257A HEAVY RIGHT
    BoxSpec { shape: Shape::Line, weights: [0, 2, 0, 0], dashes: 0 }, // U+257B HEAVY DOWN
    BoxSpec { shape: Shape::Line, weights: [0, 0, 1, 2], dashes: 0 }, // U+257C LIGHT LEFT AND HEAVY RIGHT
    BoxSpec { shape: Shape::Line, weights: [1, 2, 0, 0], dashes: 0 }, // U+257D LIGHT UP AND HEAVY DOWN
    BoxSpec { shape: Shape::Line, weights: [0, 0, 2, 1], dashes: 0 }, // U+257E HEAVY LEFT AND LIGHT RIGHT
    BoxSpec { shape: Shape::Line, weights: [2, 1, 0, 0], dashes: 0 }, // U+257F HEAVY UP AND LIGHT DOWN
];

/// The characters this module draws itself. Anything outside these ranges is
/// the font's job.
pub fn is_synthesised(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259F}')
}

/// Stroke thickness for the "light" weight, in pixels, derived from the cell
/// height so that a 12 pt and a 24 pt terminal both look deliberate.
fn light_stroke(height: u16) -> i32 {
    ((height as i32 + 8) / 16).max(1)
}

/// The rails a weight is drawn as: `(offset from centre, thickness)`.
///
/// A double line is genuinely two thin rails, not one thick one — which is why
/// its junctions have the little gaps that make `╬` look like `╬`.
fn rails(weight: u8, light: i32) -> Vec<(i32, i32)> {
    match weight {
        1 => vec![(0, light)],
        2 => vec![(0, (light * 2).max(light + 1))],
        3 => vec![(-light, light), (light, light)],
        _ => Vec::new(),
    }
}

/// Renders a synthesised glyph, or `None` if this character is not one.
pub fn render(ch: char, width: u16, height: u16) -> Option<Coverage> {
    if width == 0 || height == 0 {
        return None;
    }
    match ch as u32 {
        cp @ 0x2500..=0x257F => Some(render_box(BOX_TABLE[(cp - 0x2500) as usize], width, height)),
        0x2580..=0x259F => Some(render_block(ch, width, height)),
        _ => None,
    }
}

fn render_box(spec: BoxSpec, width: u16, height: u16) -> Coverage {
    let mut cov = Coverage::new(width, height);
    let light = light_stroke(height);

    match spec.shape {
        Shape::Diagonal(kind) => {
            draw_diagonals(&mut cov, kind, light);
            return cov;
        }
        Shape::Arc => {
            draw_arc(&mut cov, spec.weights, light);
            return cov;
        }
        Shape::Line => {}
    }

    let (w, h) = (width as i32, height as i32);
    let (cx, cy) = (w / 2, h / 2);

    let up = rails(spec.weights[0], light);
    let down = rails(spec.weights[1], light);
    let left = rails(spec.weights[2], light);
    let right = rails(spec.weights[3], light);

    // The hub is the bounding box of the perpendicular strokes. A rail that
    // terminates at the hub is what turns four independent strokes into a
    // corner that looks drawn rather than assembled.
    let hub_x = span(cx, &up, &down);
    let hub_y = span(cy, &left, &right);

    for &(offset, thickness) in &left {
        let y0 = cy + offset - thickness / 2;
        let end = if rail_at(&right, offset) {
            w // continuous across the cell
        } else if offset <= 0 {
            hub_x.1 // the rail on the side away from the opening caps the hub
        } else {
            hub_x.0
        };
        draw_run(&mut cov, spec.dashes, true, 0, end, y0, y0 + thickness, w);
    }
    for &(offset, thickness) in &right {
        let y0 = cy + offset - thickness / 2;
        if rail_at(&left, offset) {
            continue; // already drawn as one continuous run
        }
        let start = if offset <= 0 { hub_x.0 } else { hub_x.1 };
        draw_run(&mut cov, spec.dashes, true, start, w, y0, y0 + thickness, w);
    }
    for &(offset, thickness) in &up {
        let x0 = cx + offset - thickness / 2;
        let end = if rail_at(&down, offset) {
            h
        } else if offset <= 0 {
            hub_y.1
        } else {
            hub_y.0
        };
        draw_run(&mut cov, spec.dashes, false, x0, x0 + thickness, 0, end, h);
    }
    for &(offset, thickness) in &down {
        let x0 = cx + offset - thickness / 2;
        if rail_at(&up, offset) {
            continue;
        }
        let start = if offset <= 0 { hub_y.0 } else { hub_y.1 };
        draw_run(&mut cov, spec.dashes, false, x0, x0 + thickness, start, h, h);
    }

    cov
}

fn rail_at(rails: &[(i32, i32)], offset: i32) -> bool {
    rails.iter().any(|&(o, _)| o == offset)
}

/// Outer bounds of the rails perpendicular to a stroke, in pixels.
fn span(centre: i32, a: &[(i32, i32)], b: &[(i32, i32)]) -> (i32, i32) {
    let mut lo = centre;
    let mut hi = centre;
    for &(offset, thickness) in a.iter().chain(b) {
        lo = lo.min(centre + offset - thickness / 2);
        hi = hi.max(centre + offset - thickness / 2 + thickness);
    }
    (lo, hi)
}

/// Draws one rail, honouring the dash pattern.
#[allow(clippy::too_many_arguments)]
fn draw_run(
    cov: &mut Coverage,
    dashes: u8,
    horizontal: bool,
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    extent: i32,
) {
    if dashes == 0 {
        cov.fill_rect(x0, y0, x1, y1);
        return;
    }
    // Dashes are laid out across the whole cell, not across the drawn run, so
    // that consecutive dashed cells line up into one continuous pattern
    // instead of restarting at every cell boundary.
    let n = dashes as i32;
    let period = extent as f32 / n as f32;
    let ink = period * 0.6;
    for i in 0..n {
        let start = (i as f32 * period).round() as i32;
        let stop = (i as f32 * period + ink).round() as i32;
        if horizontal {
            cov.fill_rect(x0.max(start), y0, x1.min(stop), y1);
        } else {
            cov.fill_rect(x0, y0.max(start), x1, y1.min(stop));
        }
    }
}

fn draw_diagonals(cov: &mut Coverage, kind: Diagonal, light: i32) {
    let (w, h) = (cov.width as f32, cov.height as f32);
    let half = light as f32 / 2.0;
    for y in 0..cov.height as i32 {
        for x in 0..cov.width as i32 {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Perpendicular distance to each diagonal, normalised by the line
            // length so that a tall cell does not get a thinner-looking slash.
            let d1 = ((px / w) - (py / h)).abs() * (w * h) / (w * w + h * h).sqrt();
            let d2 = ((px / w) + (py / h) - 1.0).abs() * (w * h) / (w * w + h * h).sqrt();
            let d = match kind {
                Diagonal::UpLeftToLowRight => d1,
                Diagonal::UpRightToLowLeft => d2,
                Diagonal::Cross => d1.min(d2),
            };
            let alpha = (1.0 - (d - half + 0.5)).clamp(0.0, 1.0);
            if alpha > 0.0 {
                cov.put(x, y, (alpha * 255.0).round() as u8);
            }
        }
    }
}

fn draw_arc(cov: &mut Coverage, weights: [u8; 4], light: i32) {
    let (w, h) = (cov.width as f32, cov.height as f32);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (up, down, left, right) =
        (weights[0] > 0, weights[1] > 0, weights[2] > 0, weights[3] > 0);

    // The arc's centre sits diagonally opposite the corner, offset toward
    // whichever two directions the stubs leave by.
    let rx = if right { w - cx } else { cx };
    let ry = if down { h - cy } else { cy };
    let ox = if right { cx + rx } else { cx - rx };
    let oy = if down { cy + ry } else { cy - ry };

    let half = light as f32 / 2.0;
    let scale = rx.min(ry).max(1.0);
    for y in 0..cov.height as i32 {
        for x in 0..cov.width as i32 {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = (px - ox) / rx;
            let dy = (py - oy) / ry;
            // Only the quadrant that faces the two stubs.
            let in_quadrant = ((right && px <= ox) || (left && px >= ox))
                && ((down && py <= oy) || (up && py >= oy));
            if !in_quadrant {
                continue;
            }
            let radial = (dx * dx + dy * dy).sqrt();
            let distance = (radial - 1.0).abs() * scale;
            let alpha = (1.0 - (distance - half + 0.5)).clamp(0.0, 1.0);
            if alpha > 0.0 {
                cov.put(x, y, (alpha * 255.0).round() as u8);
            }
        }
    }
}

fn render_block(ch: char, width: u16, height: u16) -> Coverage {
    let mut cov = Coverage::new(width, height);
    let (w, h) = (width as i32, height as i32);
    // Eighths are rounded from the exact fraction so that stacking two
    // complementary blocks covers the cell with no gap and no overlap.
    let eighth_h = |n: i32| (h * n + 4) / 8;
    let eighth_w = |n: i32| (w * n + 4) / 8;

    match ch as u32 {
        0x2580 => cov.fill_rect(0, 0, w, h / 2),                     // upper half
        cp @ 0x2581..=0x2587 => {
            let n = (cp - 0x2580) as i32; // lower 1/8 .. 7/8
            cov.fill_rect(0, h - eighth_h(n), w, h);
        }
        0x2588 => cov.fill_rect(0, 0, w, h),                          // full
        cp @ 0x2589..=0x258F => {
            let n = 8 - (cp - 0x2588) as i32; // left 7/8 .. 1/8
            cov.fill_rect(0, 0, eighth_w(n), h);
        }
        0x2590 => cov.fill_rect(w / 2, 0, w, h),                      // right half
        0x2591 => shade(&mut cov, 64),
        0x2592 => shade(&mut cov, 128),
        0x2593 => shade(&mut cov, 192),
        0x2594 => cov.fill_rect(0, 0, w, eighth_h(1)),                // upper 1/8
        0x2595 => cov.fill_rect(w - eighth_w(1), 0, w, h),            // right 1/8
        0x2596 => cov.fill_rect(0, h / 2, w / 2, h),                  // lower left
        0x2597 => cov.fill_rect(w / 2, h / 2, w, h),                  // lower right
        0x2598 => cov.fill_rect(0, 0, w / 2, h / 2),                  // upper left
        0x2599 => {
            cov.fill_rect(0, 0, w / 2, h);
            cov.fill_rect(0, h / 2, w, h);
        }
        0x259A => {
            cov.fill_rect(0, 0, w / 2, h / 2);
            cov.fill_rect(w / 2, h / 2, w, h);
        }
        0x259B => {
            cov.fill_rect(0, 0, w, h / 2);
            cov.fill_rect(0, 0, w / 2, h);
        }
        0x259C => {
            cov.fill_rect(0, 0, w, h / 2);
            cov.fill_rect(w / 2, 0, w, h);
        }
        0x259D => cov.fill_rect(w / 2, 0, w, h / 2),                  // upper right
        0x259E => {
            cov.fill_rect(w / 2, 0, w, h / 2);
            cov.fill_rect(0, h / 2, w / 2, h);
        }
        0x259F => {
            cov.fill_rect(w / 2, 0, w, h);
            cov.fill_rect(0, h / 2, w, h);
        }
        _ => {}
    }
    cov
}

/// A flat alpha rather than a dither pattern.
///
/// Dithered shades alias badly under Retina scaling and turn into moiré when a
/// TUI scrolls them; a flat coverage reads the same at every size.
fn shade(cov: &mut Coverage, alpha: u8) {
    for v in &mut cov.data {
        *v = alpha;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u16 = 9;
    const H: u16 = 18;

    fn draw(ch: char) -> Coverage {
        render(ch, W, H).unwrap_or_else(|| panic!("{ch:?} should be synthesised"))
    }

    #[test]
    fn the_table_covers_the_whole_box_drawing_block() {
        assert_eq!(BOX_TABLE.len(), 128);
        for cp in 0x2500u32..=0x259F {
            let ch = char::from_u32(cp).unwrap();
            assert!(is_synthesised(ch), "U+{cp:04X} is not claimed");
            assert!(render(ch, W, H).is_some(), "U+{cp:04X} did not render");
        }
    }

    #[test]
    fn ordinary_text_is_left_to_the_font() {
        assert!(!is_synthesised('A'));
        assert!(!is_synthesised('\u{4E16}'));
        assert_eq!(render('A', W, H), None);
    }

    #[test]
    fn a_horizontal_line_spans_the_full_width_and_nothing_else() {
        let c = draw('\u{2500}');
        let mid = H / 2;
        for x in 0..W {
            assert_ne!(c.get(x, mid), 0, "column {x} of a horizontal rule is empty");
        }
        assert_eq!(c.get(W / 2, 0), 0, "a horizontal rule must not reach the top edge");
    }

    #[test]
    fn a_vertical_line_spans_the_full_height() {
        let c = draw('\u{2502}');
        for y in 0..H {
            assert_ne!(c.get(W / 2, y), 0, "row {y} of a vertical rule is empty");
        }
    }

    #[test]
    fn horizontal_lines_tile_without_a_seam() {
        // The entire reason this module exists. Two cells side by side must
        // join: the rightmost column of one and the leftmost of the next are
        // both inked, at the same row.
        let c = draw('\u{2500}');
        let mid = H / 2;
        assert_ne!(c.get(0, mid), 0, "left edge is not inked — a seam would show");
        assert_ne!(c.get(W - 1, mid), 0, "right edge is not inked — a seam would show");
    }

    #[test]
    fn vertical_lines_tile_without_a_seam() {
        let c = draw('\u{2502}');
        let mid = W / 2;
        assert_ne!(c.get(mid, 0), 0, "top edge is not inked");
        assert_ne!(c.get(mid, H - 1), 0, "bottom edge is not inked");
    }

    #[test]
    fn a_corner_reaches_exactly_two_edges() {
        // ┌ opens down and right, so it must touch those two edges and neither
        // of the others.
        let c = draw('\u{250C}');
        assert_ne!(c.get(W - 1, H / 2), 0, "no stroke reaching the right edge");
        assert_ne!(c.get(W / 2, H - 1), 0, "no stroke reaching the bottom edge");
        assert_eq!(c.get(0, H / 2), 0, "stroke leaked to the left edge");
        assert_eq!(c.get(W / 2, 0), 0, "stroke leaked to the top edge");
    }

    #[test]
    fn all_four_corners_face_the_right_way() {
        for (ch, right, down) in [
            ('\u{250C}', true, true),
            ('\u{2510}', false, true),
            ('\u{2514}', true, false),
            ('\u{2518}', false, false),
        ] {
            let c = draw(ch);
            assert_eq!(c.get(W - 1, H / 2) != 0, right, "{ch:?} right edge");
            assert_eq!(c.get(0, H / 2) != 0, !right, "{ch:?} left edge");
            assert_eq!(c.get(W / 2, H - 1) != 0, down, "{ch:?} bottom edge");
            assert_eq!(c.get(W / 2, 0) != 0, !down, "{ch:?} top edge");
        }
    }

    #[test]
    fn a_cross_reaches_all_four_edges() {
        let c = draw('\u{253C}');
        assert_ne!(c.get(0, H / 2), 0);
        assert_ne!(c.get(W - 1, H / 2), 0);
        assert_ne!(c.get(W / 2, 0), 0);
        assert_ne!(c.get(W / 2, H - 1), 0);
    }

    #[test]
    fn a_heavy_line_is_thicker_than_a_light_one() {
        let light = draw('\u{2500}');
        let heavy = draw('\u{2501}');
        let ink = |c: &Coverage| c.data.iter().filter(|&&v| v > 0).count();
        assert!(ink(&heavy) > ink(&light), "heavy must lay down more ink than light");
    }

    #[test]
    fn a_double_line_is_two_rails_with_a_gap_between_them() {
        let c = draw('\u{2550}');
        let column: Vec<u8> = (0..H).map(|y| c.get(W / 2, y)).collect();
        // Count runs of ink down the middle column: a double rule has two.
        let mut runs = 0;
        let mut inside = false;
        for v in column {
            if v > 0 && !inside {
                runs += 1;
                inside = true;
            } else if v == 0 {
                inside = false;
            }
        }
        assert_eq!(runs, 2, "a double rule must be two separate rails");
    }

    #[test]
    fn a_dashed_line_has_gaps_and_a_solid_one_does_not() {
        let solid = draw('\u{2500}');
        let dashed = draw('\u{2504}');
        let mid = H / 2;
        let holes = |c: &Coverage| (0..W).filter(|&x| c.get(x, mid) == 0).count();
        assert_eq!(holes(&solid), 0);
        assert!(holes(&dashed) > 0, "a triple-dash rule must have visible gaps");
    }

    #[test]
    fn arcs_reach_their_two_edges_and_curve_between_them() {
        let c = draw('\u{256D}'); // ╭ — down and right
        assert_ne!(c.get(W - 1, H / 2), 0, "arc must meet the right edge");
        assert_ne!(c.get(W / 2, H - 1), 0, "arc must meet the bottom edge");
        // The defining property of an arc: the corner point itself is empty.
        assert_eq!(c.get(W / 2, H / 2), 0, "an arc must not pass through the cell centre");
    }

    #[test]
    fn diagonals_run_corner_to_corner() {
        let slash = draw('\u{2571}'); // ╱ upper-right to lower-left
        assert_ne!(slash.get(W - 1, 0), 0);
        assert_ne!(slash.get(0, H - 1), 0);
        assert_eq!(slash.get(0, 0), 0);

        let cross = draw('\u{2573}');
        assert_ne!(cross.get(0, 0), 0);
        assert_ne!(cross.get(W - 1, 0), 0);
    }

    #[test]
    fn a_full_block_covers_every_pixel() {
        let c = draw('\u{2588}');
        assert!(c.data.iter().all(|&v| v == 255));
    }

    #[test]
    fn complementary_half_blocks_tile_the_cell_exactly() {
        // Upper half plus lower half must cover the cell with no gap row and
        // no doubled row — this is what a progress bar is built out of.
        let upper = draw('\u{2580}');
        let lower = render('\u{2584}', W, H).unwrap(); // lower half = lower 4/8
        for y in 0..H {
            for x in 0..W {
                let covered = (upper.get(x, y) > 0) as u8 + (lower.get(x, y) > 0) as u8;
                assert_eq!(covered, 1, "pixel ({x},{y}) covered {covered} times");
            }
        }
    }

    #[test]
    fn the_eighth_blocks_increase_monotonically() {
        let ink = |ch: char| draw(ch).data.iter().filter(|&&v| v > 0).count();
        let mut previous = 0;
        for cp in 0x2581u32..=0x2588 {
            let n = ink(char::from_u32(cp).unwrap());
            assert!(n > previous, "U+{cp:04X} did not grow: {n} <= {previous}");
            previous = n;
        }
    }

    #[test]
    fn the_shades_are_ordered_light_to_dark() {
        let alpha = |ch: char| draw(ch).data[0];
        assert!(alpha('\u{2591}') < alpha('\u{2592}'));
        assert!(alpha('\u{2592}') < alpha('\u{2593}'));
        assert!(alpha('\u{2593}') < 255);
    }

    #[test]
    fn quadrants_cover_exactly_their_own_quarter() {
        let c = draw('\u{2598}'); // upper left
        assert_ne!(c.get(0, 0), 0);
        assert_eq!(c.get(W - 1, 0), 0);
        assert_eq!(c.get(0, H - 1), 0);
    }

    #[test]
    fn every_glyph_renders_at_a_tiny_cell_without_panicking() {
        // A 3x4 cell is absurd, but a font-size slider can produce one and a
        // panic in the atlas takes the whole terminal down.
        for cp in 0x2500u32..=0x259F {
            let ch = char::from_u32(cp).unwrap();
            let c = render(ch, 3, 4).unwrap();
            assert_eq!(c.data.len(), 12);
        }
    }

    #[test]
    fn a_stroke_is_never_blank_when_a_weight_was_requested() {
        for cp in 0x2500u32..=0x257F {
            let ch = char::from_u32(cp).unwrap();
            assert!(!draw(ch).is_blank(), "U+{cp:04X} rendered nothing at all");
        }
    }
}
