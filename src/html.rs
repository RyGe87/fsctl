//! HTML, made readable.
//!
//! The reading is not ours: `textutil` on macOS hands over WebKit's own
//! importer, which already knows entities, encodings, tables, and that a
//! `<script>` is not text. Linux has `w3m`, `lynx` or `html2text` for the same
//! job. We take their plain text and give back the one thing a terminal misses
//! afterwards — which lines were headings — by looking for those in the source.
//!
//! So no parser here either. A regex-shaped scan for `<h1>`…`<h6>` is enough to
//! decide what to make bold, and being wrong about it costs a bold line.

use std::path::Path;
use std::process::Command;

use crate::markdown::Styled;
use crate::term::{Color, Modifier, Style};
use crate::toolbox::{self, HtmlTool};

pub fn is_html(path: &Path) -> bool {
    matches!(
        path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref(),
        Some("html" | "htm" | "xhtml")
    )
}

/// The page as text, plus the source it came from.
pub fn render(path: &Path) -> Result<(Vec<Styled>, Vec<String>, &'static str), String> {
    let (kind, program) = toolbox::get()
        .html
        .clone()
        .ok_or("no tool here that reads html")?;
    let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;

    let mut command = Command::new(&program);
    match kind {
        HtmlTool::Textutil => {
            command.args(["-convert", "txt", "-stdout"]).arg(path);
        }
        HtmlTool::W3m => {
            command.args(["-dump", "-T", "text/html"]).arg(path);
        }
        HtmlTool::Lynx => {
            command.args(["-dump", "-nolist"]).arg(path);
        }
        HtmlTool::Html2text => {
            command.arg(path);
        }
    }
    let out = command.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or("could not be read")
            .trim()
            .to_string());
    }

    let headings = headings_in(&source);
    let text = String::from_utf8_lossy(&out.stdout);
    Ok((style(&text, &headings), source.lines().map(|l| l.to_string()).collect(), kind.name()))
}

/// The text of every `<h1>`…`<h6>`, flattened the way the converter would have
/// flattened it, so the two can be compared line for line.
fn headings_in(source: &str) -> Vec<String> {
    let lower = source.to_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = lower[at..].find("<h") {
        let start = at + open;
        let Some(level) = lower[start + 2..].chars().next() else {
            break;
        };
        if !('1'..='6').contains(&level) {
            at = start + 2;
            continue;
        }
        let Some(head_end) = lower[start..].find('>') else {
            break;
        };
        let body_start = start + head_end + 1;
        let Some(close) = lower[body_start..].find("</h") else {
            at = body_start;
            continue;
        };
        let text = strip_tags(&source[body_start..body_start + close]);
        if !text.is_empty() {
            out.push(text);
        }
        at = body_start + close;
    }
    out
}

/// Tags out, entities in — only the handful that turn up inside a heading.
fn strip_tags(fragment: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in fragment.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The converter's text, tidied and marked: headings bold, bullets aligned,
/// and never more than one empty line in a row.
fn style(text: &str, headings: &[String]) -> Vec<Styled> {
    let mut out: Vec<Styled> = Vec::new();
    let mut blank_before = false;
    for line in text.lines() {
        // textutil writes a bullet as tab-dot-tab; a terminal wants two cells.
        let cleaned = line.replace("\t•\t", "• ").replace('\t', "    ");
        let trimmed = cleaned.trim_end();
        if trimmed.trim().is_empty() {
            if blank_before {
                continue;
            }
            blank_before = true;
            out.push(vec![(String::new(), Style::new())]);
            continue;
        }
        blank_before = false;
        let is_heading = headings.iter().any(|h| h == trimmed.trim());
        let style = if is_heading {
            Style::new().add_modifier(Modifier::BOLD)
        } else if trimmed.starts_with("• ") {
            Style::new()
        } else {
            Style::new()
        };
        if trimmed.starts_with("• ") {
            out.push(vec![
                ("• ".to_string(), Style::new().fg(Color::Yellow)),
                (trimmed[4..].to_string(), style),
            ]);
        } else {
            out.push(vec![(trimmed.to_string(), style)]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_are_found_whatever_their_case_or_attributes() {
        let source = "<H1 class='x'>First</H1><p>no</p><h3>Second <b>bold</b></h3>";
        assert_eq!(headings_in(source), vec!["First", "Second bold"]);
    }

    #[test]
    fn entities_come_back_as_characters() {
        assert_eq!(strip_tags("a &amp; b"), "a & b");
        assert_eq!(strip_tags("<b>x</b> y"), "x y");
    }

    #[test]
    fn a_run_of_blank_lines_becomes_one() {
        let styled = style("a\n\n\n\nb", &[]);
        assert_eq!(styled.len(), 3);
    }

    #[test]
    fn a_heading_line_is_marked_and_the_rest_is_not() {
        let styled = style("Title\ntext", &["Title".to_string()]);
        assert_eq!(styled[0][0].0, "Title");
        assert_ne!(styled[0][0].1, styled[1][0].1);
    }
}
