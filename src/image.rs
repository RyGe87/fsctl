//! A picture, small enough to fit in a terminal.
//!
//! Terminal.app speaks none of the inline-image protocols and knows 256
//! colours, so a photograph arrives here as blocks: every `▀` carries two
//! pixels — its ink is the upper one, its paper the lower — which buys back the
//! vertical resolution that a character cell costs.
//!
//! The decoding is `sips`, which is already on every Mac and reads everything
//! Apple's frameworks read: png, jpeg, heic, tiff, gif, pdf. We ask it for a
//! small BMP, which is a header and then the pixels, and place those.

use std::path::Path;
use std::process::Command;

use crate::markdown::Styled;
use crate::term::{Color, Style};
use crate::toolbox::{self, ImageTool};

/// Half a block: ink on top, paper below.
const HALF: &str = "▀";

pub fn is_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "heic"
                | "heif"
                | "tif"
                | "tiff"
                | "bmp"
                | "webp"
                | "ico"
                | "icns"
                | "pdf"
        )
    )
}

/// What sips says the picture measures. Only sips needs asking: ImageMagick
/// can fit a picture into a box in the same call that converts it.
fn sips_dimensions(program: &Path, path: &Path) -> Option<(u32, u32)> {
    let out = Command::new(program)
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let read = |key: &str| -> Option<u32> {
        text.lines()
            .find(|l| l.trim_start().starts_with(key))?
            .rsplit(':')
            .next()?
            .trim()
            .parse()
            .ok()
    };
    Some((read("pixelWidth")?, read("pixelHeight")?))
}

/// The thumbnail, as rows of half-blocks, plus what it measures.
pub fn thumbnail(path: &Path, cols: usize, rows: usize) -> Result<(Vec<Styled>, String), String> {
    if cols < 4 || rows < 2 {
        return Err("not enough room".to_string());
    }
    // Two pixels to a row, and never bigger than the picture itself: blowing a
    // 16×16 icon up to full width only makes it blurry.
    let (room_w, room_h) = (cols as u32, (rows as u32) * 2);
    let (kind, program) = toolbox::get()
        .image
        .clone()
        .ok_or("no image tool on this machine")?;

    let (bytes, source) = match kind {
        ImageTool::Sips => {
            let (source_w, source_h) =
                sips_dimensions(&program, path).ok_or("sips cannot read this file")?;
            if source_w == 0 || source_h == 0 {
                return Err("no dimensions".to_string());
            }
            let scale = (room_w as f64 / source_w as f64)
                .min(room_h as f64 / source_h as f64)
                .min(1.0);
            let width = ((source_w as f64 * scale).round() as u32).max(1);
            let height = ((source_h as f64 * scale).round() as u32).max(1);

            // sips only writes to a file, so this one path needs a scratch one.
            let target =
                std::env::temp_dir().join(format!("fsctl-thumb-{}.bmp", std::process::id()));
            let out = Command::new(&program)
                .args(["-z", &height.to_string(), &width.to_string()])
                .args(["-s", "format", "bmp"])
                .arg(path)
                .arg("--out")
                .arg(&target)
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                let _ = std::fs::remove_file(&target);
                return Err("sips could not convert this".to_string());
            }
            let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&target);
            (bytes, Some((source_w, source_h)))
        }
        ImageTool::ImageMagick => {
            // `WxH>` means "fit inside, and only ever shrink" — the same rule,
            // stated to the tool instead of computed for it. And it writes to
            // its output, so there is no scratch file at all.
            let mut command = Command::new(&program);
            if program.to_string_lossy().ends_with("magick") {
                command.arg("convert");
            }
            let out = command
                .arg(format!("{}[0]", path.display()))
                .args(["-resize", &format!("{room_w}x{room_h}>")])
                .arg("BMP3:-")
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next()
                    .unwrap_or("imagemagick could not convert this")
                    .rsplit(": ")
                    .next()
                    .unwrap_or("imagemagick could not convert this")
                    .to_string());
            }
            (out.stdout, None)
        }
    };

    let picture = parse_bmp(&bytes)?;
    let note = match source {
        Some((w, h)) => format!("{w}×{h} · thumbnail {}×{}", picture.width, picture.height),
        None => format!("thumbnail {}×{}", picture.width, picture.height),
    };
    Ok((to_blocks(&picture), note))
}

struct Picture {
    width: usize,
    height: usize,
    /// Row-major, top row first: red, green, blue, alpha.
    pixels: Vec<[u8; 4]>,
}

impl Picture {
    fn at(&self, x: usize, y: usize) -> [u8; 4] {
        self.pixels
            .get(y * self.width + x)
            .copied()
            .unwrap_or([0, 0, 0, 0])
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Enough of BMP for what sips writes: uncompressed 24- or 32-bit pixels,
/// stored bottom-up unless the height says otherwise.
fn parse_bmp(bytes: &[u8]) -> Result<Picture, String> {
    if bytes.len() < 54 || &bytes[..2] != b"BM" {
        return Err("not a BMP".to_string());
    }
    let start = u32_at(bytes, 10) as usize;
    let width = u32_at(bytes, 18) as i32;
    let raw_height = u32_at(bytes, 22) as i32;
    let bpp = u16::from_le_bytes([bytes[28], bytes[29]]) as usize;
    if width <= 0 || raw_height == 0 || !(bpp == 24 || bpp == 32) {
        return Err(format!("BMP with {bpp} bits is not read"));
    }
    let width = width as usize;
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs() as usize;

    // Rows are padded to a multiple of four bytes.
    let stride = (width * bpp / 8).div_ceil(4) * 4;
    if start + stride * height > bytes.len() {
        return Err("BMP is truncated".to_string());
    }

    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        let source_row = if top_down { row } else { height - 1 - row };
        let base = start + source_row * stride;
        for x in 0..width {
            let at = base + x * bpp / 8;
            // BMP stores blue first.
            let (b, g, r) = (bytes[at], bytes[at + 1], bytes[at + 2]);
            let a = if bpp == 32 { bytes[at + 3] } else { 255 };
            pixels.push([r, g, b, a]);
        }
    }
    Ok(Picture {
        width,
        height,
        pixels,
    })
}

/// Two pixel rows per line of text.
fn to_blocks(picture: &Picture) -> Vec<Styled> {
    let mut out = Vec::new();
    let mut y = 0;
    while y < picture.height {
        let mut line: Styled = Vec::new();
        for x in 0..picture.width {
            let top = picture.at(x, y);
            let bottom = if y + 1 < picture.height {
                picture.at(x, y + 1)
            } else {
                [0, 0, 0, 0]
            };
            line.push(cell(top, bottom));
        }
        out.push(line);
        y += 2;
    }
    out
}

/// One character for two pixels — and for the see-through ones, whatever is
/// behind the terminal, which is how an icon keeps its shape.
fn cell(top: [u8; 4], bottom: [u8; 4]) -> (String, Style) {
    let solid = |p: [u8; 4]| p[3] >= 128;
    match (solid(top), solid(bottom)) {
        (true, true) => (
            HALF.to_string(),
            Style::new()
                .fg(Color::Indexed(palette(top)))
                .bg(Color::Indexed(palette(bottom))),
        ),
        (true, false) => (
            HALF.to_string(),
            Style::new().fg(Color::Indexed(palette(top))),
        ),
        // The lower half alone: the same block, upside down.
        (false, true) => (
            "▄".to_string(),
            Style::new().fg(Color::Indexed(palette(bottom))),
        ),
        (false, false) => (" ".to_string(), Style::new()),
    }
}

/// The nearest slot in the terminal's 256-colour palette: a 6×6×6 cube for
/// colour, a 24-step ramp for grey, which is where the cube is weakest.
fn palette(pixel: [u8; 4]) -> u8 {
    let [r, g, b, _] = pixel;
    let spread = r.max(g).max(b) as i32 - r.min(g).min(b) as i32;
    if spread < 10 {
        let level = (r as u16 + g as u16 + b as u16) / 3;
        if level < 8 {
            return 16;
        }
        if level > 247 {
            return 231;
        }
        return 232 + ((level - 8) * 24 / 240) as u8;
    }
    16 + 36 * step(r) + 6 * step(g) + step(b)
}

/// Which of the cube's six levels a channel lands on.
fn step(value: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut best = 0;
    let mut distance = i32::MAX;
    for (i, level) in LEVELS.iter().enumerate() {
        let d = (value as i32 - *level as i32).abs();
        if d < distance {
            distance = d;
            best = i as u8;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_uses_the_grey_ramp() {
        // Mid grey lands in the ramp, not in the cube.
        assert!(palette([128, 128, 128, 255]) >= 232);
        // Pure red does not.
        assert_eq!(palette([255, 0, 0, 255]), 16 + 36 * 5);
    }

    #[test]
    fn the_darkest_and_lightest_are_the_cube_ends() {
        assert_eq!(palette([0, 0, 0, 255]), 16);
        assert_eq!(palette([255, 255, 255, 255]), 231);
    }

    #[test]
    fn a_see_through_pixel_leaves_the_cell_alone() {
        let (glyph, style) = cell([0, 0, 0, 0], [0, 0, 0, 0]);
        assert_eq!(glyph, " ");
        assert_eq!(style.fg, None);
        assert_eq!(style.bg, None);
    }
}
