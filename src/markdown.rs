//! Markdown, made readable in a terminal.
//!
//! The one format we lay out ourselves, because macOS ships nothing that does
//! it — `textutil` speaks RTF and HTML, not this. So the rule of the house
//! bends exactly this far and no further: no reflowing, no rewriting, no
//! parser beyond what a glance needs.
//!
//! Every source line stays one line on screen. That keeps the line numbers
//! honest, and it means `t` shows you the same file you were just looking at
//! rather than a different shape of it.

use crate::term::{Color, Modifier, Style};

/// A rendered line: pieces of text, each with how it should look.
pub type Styled = Vec<(String, Style)>;

fn plain() -> Style {
    Style::new()
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn code() -> Style {
    Style::new().fg(Color::Cyan)
}

pub fn render(lines: &[String]) -> Vec<Styled> {
    let mut out = Vec::with_capacity(lines.len());
    let mut fenced = false;

    for line in lines {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];

        // A fence flips the mode, and is itself just scenery.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            out.push(vec![(line.clone(), dim())]);
            continue;
        }
        if fenced {
            out.push(vec![(line.clone(), code())]);
            continue;
        }

        // A rule is a rule, whichever of the three spellings it uses.
        let bare: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if bare.len() >= 3
            && (bare.chars().all(|c| c == '-')
                || bare.chars().all(|c| c == '*')
                || bare.chars().all(|c| c == '_'))
        {
            out.push(vec![(format!("{indent}{}", "─".repeat(40)), dim())]);
            continue;
        }

        if let Some(rest) = heading(trimmed) {
            let mut styled: Styled = vec![(indent.to_string(), plain())];
            styled.extend(inline(rest, Style::new().add_modifier(Modifier::BOLD)));
            out.push(styled);
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("> ").or(trimmed.strip_prefix(">")) {
            let mut styled: Styled = vec![(format!("{indent}│ "), dim())];
            styled.extend(inline(rest, dim()));
            out.push(styled);
            continue;
        }

        // Bullets become one bullet, whichever marker was typed.
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or(trimmed.strip_prefix("* "))
            .or(trimmed.strip_prefix("+ "))
        {
            let mut styled: Styled = vec![(format!("{indent}• "), Style::new().fg(Color::Yellow))];
            styled.extend(inline(rest, plain()));
            out.push(styled);
            continue;
        }

        let mut styled: Styled = vec![(indent.to_string(), plain())];
        styled.extend(inline(trimmed, plain()));
        out.push(styled);
    }
    out
}

/// `## Heading` → `Heading`, up to six hashes and only with a space behind them.
fn heading(trimmed: &str) -> Option<&str> {
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    trimmed[hashes..]
        .strip_prefix(' ')
        .map(|rest| rest.trim_end())
}

/// The markers inside a line: `**strong**`, `_emphasis_`, `` `code` `` and
/// `[text](url)`. Everything else is left exactly as it was typed — this is a
/// preview, not an editor, and a half-understood construct should still read.
fn inline(text: &str, base: Style) -> Styled {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Styled = Vec::new();
    let mut buffer = String::new();
    let mut i = 0;

    let flush = |buffer: &mut String, out: &mut Styled| {
        if !buffer.is_empty() {
            out.push((std::mem::take(buffer), base));
        }
    };

    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();

        if let Some(inner) = between(&rest, "**") {
            flush(&mut buffer, &mut out);
            out.push((inner.clone(), base.add_modifier(Modifier::BOLD)));
            i += inner.chars().count() + 4;
            continue;
        }
        if let Some(inner) = between(&rest, "`") {
            flush(&mut buffer, &mut out);
            out.push((inner.clone(), code()));
            i += inner.chars().count() + 2;
            continue;
        }
        // A single star or underscore only counts when it is not in the middle
        // of a word: snake_case_names are not emphasis.
        if (chars[i] == '*' || chars[i] == '_')
            && (i == 0 || chars[i - 1].is_whitespace())
            && let Some(inner) = between(&rest, &chars[i].to_string())
            && !inner.starts_with(' ')
        {
            flush(&mut buffer, &mut out);
            out.push((inner.clone(), base.add_modifier(Modifier::ITALIC)));
            i += inner.chars().count() + 2;
            continue;
        }
        if chars[i] == '['
            && let Some((label, url)) = link(&rest)
        {
            flush(&mut buffer, &mut out);
            out.push((label.clone(), base.add_modifier(Modifier::BOLD)));
            out.push((format!(" ({url})"), dim()));
            i += label.chars().count() + url.chars().count() + 4;
            continue;
        }

        buffer.push(chars[i]);
        i += 1;
    }
    flush(&mut buffer, &mut out);
    out
}

/// What sits between the next pair of `marker`s, if the pair closes on this
/// line and holds something.
fn between(text: &str, marker: &str) -> Option<String> {
    let rest = text.strip_prefix(marker)?;
    let end = rest.find(marker)?;
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

/// `[text](url)` → the two halves.
fn link(text: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix('[')?;
    let close = rest.find("](")?;
    let label = rest[..close].to_string();
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    if label.is_empty() {
        return None;
    }
    Some((label, after[..end].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(styled: &Styled) -> String {
        styled.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn headings_lose_their_hashes() {
        assert_eq!(heading("## Heading"), Some("Heading"));
        assert_eq!(heading("#not a heading"), None);
        assert_eq!(heading("####### too deep"), None);
    }

    #[test]
    fn markers_disappear_but_the_words_stay() {
        assert_eq!(text(&inline("a **strong** word", plain())), "a strong word");
        assert_eq!(
            text(&inline("with `code` in it", plain())),
            "with code in it"
        );
    }

    #[test]
    fn snake_case_is_not_emphasis() {
        let line = "a pack_id stays whole";
        assert_eq!(text(&inline(line, plain())), line);
    }

    #[test]
    fn a_link_keeps_its_target_visible() {
        assert_eq!(
            text(&inline("[docs](https://x.be)", plain())),
            "docs (https://x.be)"
        );
    }

    #[test]
    fn every_source_line_stays_one_line() {
        let source: Vec<String> = ["# Heading", "", "- point", "```", "code", "```", "> quote"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(render(&source).len(), source.len());
    }
}
