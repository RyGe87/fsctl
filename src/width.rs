//! How wide a string draws.
//!
//! `term.rs` came from sshctl, where every string on screen was a host name:
//! ASCII, one cell per character. File names are not so polite. One emoji in a
//! folder name and every column to its right slides out of place, so we need to
//! know which characters take two cells.
//!
//! This is a compact table, not the full Unicode width database — the ranges
//! that actually turn up in file names, and nothing else. A character we guess
//! wrong costs one cell of alignment, never correctness.

/// Cells a single character occupies, ignoring what follows it.
pub fn char_width(c: char) -> usize {
    let cp = c as u32;
    // Combining marks and zero-width joiners ride along with the character
    // before them.
    const ZERO: &[(u32, u32)] = &[
        (0x0300, 0x036F), // combining diacritics — "é" typed as e + accent
        (0x200B, 0x200F), // zero-width space through the direction marks
        (0xFE00, 0xFE0F), // variation selectors
        (0x1AB0, 0x1AFF),
        (0x20D0, 0x20FF),
    ];
    // Anything that a terminal draws across two cells.
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115F), // Hangul Jamo
        (0x2E80, 0x303E), // CJK radicals, Kangxi, punctuation
        (0x3041, 0x33FF), // kana through CJK compatibility
        (0x3400, 0x4DBF), // CJK extension A
        (0x4E00, 0x9FFF), // CJK unified
        (0xA000, 0xA4CF), // Yi
        (0xAC00, 0xD7A3), // Hangul syllables
        (0xF900, 0xFAFF), // CJK compatibility ideographs
        (0xFE30, 0xFE6F), // CJK compatibility forms
        (0xFF00, 0xFF60), // fullwidth forms
        (0xFFE0, 0xFFE6),
        (0x1F300, 0x1F5FF), // symbols and pictographs
        (0x1F600, 0x1F64F), // emoticons
        (0x1F680, 0x1F6FF), // transport
        (0x1F900, 0x1F9FF), // supplemental symbols
        (0x1FA70, 0x1FAFF), // extended-A
        (0x20000, 0x2FFFD), // CJK extension B and beyond
    ];
    if c == '\t' || (cp < 0x20) {
        return 0;
    }
    if ZERO.iter().any(|(lo, hi)| (*lo..=*hi).contains(&cp)) {
        return 0;
    }
    if WIDE.iter().any(|(lo, hi)| (*lo..=*hi).contains(&cp)) {
        2
    } else {
        1
    }
}

/// Cells a string occupies.
///
/// A variation selector after a symbol is what turns "✂" into an emoji, and
/// with it the terminal switches to two cells — so the selector is not merely
/// zero width, it widens what came before.
pub fn str_width(s: &str) -> usize {
    let mut total = 0;
    let mut previous: Option<char> = None;
    for c in s.chars() {
        if c == '\u{FE0F}' {
            if let Some(p) = previous
                && char_width(p) == 1
            {
                total += 1;
            }
            previous = Some(c);
            continue;
        }
        total += char_width(c);
        previous = Some(c);
    }
    total
}

/// Cuts a string to at most `width` cells, marking the cut with an ellipsis.
pub fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if str_width(s) <= width {
        return s.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > width - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Cuts from the front instead, keeping the end.
///
/// For a path the tail is what tells you where you are: `…/products/fsctl`
/// says more than `~/Development/pro…`.
pub fn truncate_start(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if str_width(s) <= width {
        return s.to_string();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1; // the ellipsis
    for c in s.chars().rev() {
        let w = char_width(c);
        if used + w > width {
            break;
        }
        kept.push(c);
        used += w;
    }
    kept.reverse();
    format!("…{}", kept.into_iter().collect::<String>())
}

/// A window onto a line: skip `start` cells, then keep at most `width`.
///
/// A double-width character straddling the left edge is dropped rather than
/// half-drawn — half a glyph is not a thing a terminal can show.
pub fn window(s: &str, start: usize, width: usize) -> String {
    if start == 0 {
        return truncate(s, width);
    }
    let mut skipped = 0;
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if skipped < start {
            skipped += char_width(c);
            continue;
        }
        let w = char_width(c);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// Cuts to `width` cells and pads with spaces to exactly that many.
pub fn fit(s: &str, width: usize) -> String {
    let mut out = truncate(s, width);
    let pad = width.saturating_sub(str_width(&out));
    out.push_str(&" ".repeat(pad));
    out
}

/// Pads on the left instead, for numbers.
pub fn fit_right(s: &str, width: usize) -> String {
    let out = truncate(s, width);
    let pad = width.saturating_sub(str_width(&out));
    format!("{}{}", " ".repeat(pad), out)
}
