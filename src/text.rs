use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

use crate::display::one_line;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub matched: bool,
    pub bold: bool,
    pub code: bool,
}

pub type Chars = Vec<(char, Look)>;

const ELLIPSIS: (char, Look) = (
    '…',
    Look {
        matched: false,
        bold: false,
        code: false,
    },
);

pub fn marked(text: &str, tokens: &[String]) -> Chars {
    let mut chars = one_line(text)
        .chars()
        .map(|c| (c, Look::default()))
        .collect();
    mark(&mut chars, tokens);
    chars
}

fn mark(chars: &mut Chars, tokens: &[String]) {
    let lower: Vec<char> = chars
        .iter()
        .map(|(c, _)| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    for token in tokens {
        let t: Vec<char> = token.chars().collect();
        for i in 0..lower.len() {
            if (i == 0 || !lower[i - 1].is_alphanumeric()) && lower[i..].starts_with(&t) {
                chars[i..i + t.len()]
                    .iter_mut()
                    .for_each(|(_, look)| look.matched = true);
            }
        }
    }
}

pub fn markdown(text: &str, tokens: &[String], width: usize) -> Vec<Chars> {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        let line = &line.replace('\t', "    ");
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let (prefix, mut chars) = if in_code {
            let look = Look {
                code: true,
                ..Look::default()
            };
            (
                String::new(),
                line.trim_end().chars().map(|c| (c, look)).collect(),
            )
        } else {
            let nesting = " ".repeat(((line.len() - trimmed.len()) / 2).min(3) * 2);
            let (marker, body, bold) = block(trimmed);
            let mut chars = inline(body);
            if bold {
                chars.iter_mut().for_each(|(_, look)| look.bold = true);
            }
            (format!("{nesting}{marker}"), chars)
        };
        mark(&mut chars, tokens);
        let indent = prefix.chars().count();
        for (i, part) in wrap(&chars, width.saturating_sub(indent).max(8))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 {
                prefix.clone()
            } else {
                " ".repeat(indent)
            };
            let mut line: Chars = lead.chars().map(|c| (c, Look::default())).collect();
            line.extend(part);
            out.push(line);
        }
    }
    out
}

fn block(line: &str) -> (String, &str, bool) {
    let heading = line.trim_start_matches('#');
    if heading.len() < line.len() && heading.starts_with(' ') && line.len() - heading.len() <= 6 {
        return (String::new(), heading.trim_start(), true);
    }
    for bullet in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            return ("• ".into(), rest, false);
        }
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=3).contains(&digits)
        && let Some(rest) = line[digits..].strip_prefix(". ")
    {
        return (format!("{}. ", &line[..digits]), rest, false);
    }
    if let Some(rest) = line.strip_prefix("> ") {
        return (String::new(), rest, false);
    }
    (String::new(), line, false)
}

fn inline(text: &str) -> Chars {
    let mut out = Chars::new();
    let mut look = Look::default();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("**") {
            look.bold = !look.bold;
            rest = after;
        } else if c == '`' {
            look.code = !look.code;
            rest = &rest[1..];
        } else if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest[close..].find(')')
        {
            out.extend(rest[1..close].chars().map(|c| (c, look)));
            rest = &rest[close + end + 1..];
        } else {
            out.push((c, look));
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

pub fn width(chars: &[(char, Look)]) -> usize {
    chars.iter().map(|(c, _)| c.width().unwrap_or(0)).sum()
}

pub fn cut(chars: &[(char, Look)], width: usize) -> Chars {
    if self::width(chars) <= width {
        return chars.to_vec();
    }
    let mut out = Chars::new();
    let mut w = 0;
    for &(c, look) in chars {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push((c, look));
        w += cw;
    }
    if width > 0 {
        out.push(ELLIPSIS);
    }
    out
}

pub fn continued(line: &[(char, Look)], width: usize) -> Chars {
    if line.last().is_some_and(|(c, _)| *c == '…') {
        return line.to_vec();
    }
    let mut out = cut(line, self::width(line).min(width.saturating_sub(1)));
    if out.last().is_none_or(|(c, _)| *c != '…') {
        out.push(ELLIPSIS);
    }
    out
}

pub fn cut_str(s: &str, width: usize) -> String {
    let chars: Chars = s.chars().map(|c| (c, Look::default())).collect();
    cut(&chars, width).into_iter().map(|(c, _)| c).collect()
}

pub fn wrap(chars: &[(char, Look)], width: usize) -> Vec<Chars> {
    let mut lines = Vec::new();
    let mut line = Chars::new();
    let mut line_width = 0;
    for word in chars.split(|&(c, _)| c == ' ') {
        let word_width = self::width(word);
        if line_width > 0 && line_width + 1 + word_width > width {
            lines.push(std::mem::take(&mut line));
            line_width = 0;
        }
        if line_width > 0 {
            line.push((' ', Look::default()));
            line_width += 1;
        }
        for &(c, look) in word {
            let cw = c.width().unwrap_or(0);
            if line_width + cw > width {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            line.push((c, look));
            line_width += cw;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

pub fn match_style(base: Style) -> Style {
    base.fg(Color::Indexed(179))
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::DIM)
}

pub fn spans(chars: &[(char, Look)], base: Style) -> Vec<Span<'static>> {
    let style = |look: Look| {
        let mut style = base;
        if look.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if look.code {
            style = style.fg(Color::Indexed(180));
        }
        if look.matched {
            style = match_style(style);
        }
        style
    };
    let mut out = Vec::new();
    let mut run = String::new();
    let mut run_look = Look::default();
    for &(c, look) in chars {
        if look != run_look && !run.is_empty() {
            out.push(Span::styled(std::mem::take(&mut run), style(run_look)));
        }
        run_look = look;
        run.push(c);
    }
    if !run.is_empty() {
        out.push(Span::styled(run, style(run_look)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(chars: &[(char, Look)]) -> String {
        chars.iter().map(|(c, _)| c).collect()
    }

    fn lines(text: &str, width: usize) -> Vec<String> {
        markdown(text, &[], width)
            .iter()
            .map(|l| plain(l))
            .collect()
    }

    #[test]
    fn marks_word_prefixes() {
        let m = marked("Fix Stripe webhook", &["stri".into(), "hook".into()]);
        let hit: String = m
            .iter()
            .filter(|(_, l)| l.matched)
            .map(|(c, _)| c)
            .collect();
        assert_eq!(hit, "Stri");
        assert_eq!(plain(&marked("  two\n lines ", &[])), "two lines");
    }

    #[test]
    fn cutting() {
        assert_eq!(cut_str("hello", 10), "hello");
        assert_eq!(cut_str("hello world", 6), "hello…");
        assert_eq!(cut_str("日本語テキスト", 7), "日本語…");
        assert_eq!(plain(&continued(&marked("done", &[]), 10)), "done…");
        assert_eq!(plain(&continued(&marked("already…", &[]), 10)), "already…");
    }

    #[test]
    fn wrapping() {
        let text = marked("the quick brown fox jumps over the lazy dog", &[]);
        let lines: Vec<String> = wrap(&text, 10).iter().map(|l| plain(l)).collect();
        assert_eq!(
            lines,
            ["the quick", "brown fox", "jumps over", "the lazy", "dog"]
        );
        let long: Vec<String> = wrap(&marked("abcdefghijkl", &[]), 5)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(long, ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn markdown_keeps_structure() {
        let text = "## Findings\n\nThe handler isn't idempotent:\n\n- no dedup on event id\n- the charge is written before the ack, which is the part that breaks\n  - nested\n1. first\n\n```rust\nlet x = 1;\n```";
        assert_eq!(
            lines(text, 30),
            [
                "Findings",
                "The handler isn't idempotent:",
                "• no dedup on event id",
                "• the charge is written before",
                "  the ack, which is the part",
                "  that breaks",
                "  • nested",
                "1. first",
                "let x = 1;",
            ]
        );
    }

    #[test]
    fn inline_markdown_is_rendered() {
        let chars = inline("**Done.** Run `cargo test`, see [the docs](https://example.com/x).");
        assert_eq!(plain(&chars), "Done. Run cargo test, see the docs.");
        let bold: String = chars
            .iter()
            .filter(|(_, l)| l.bold)
            .map(|(c, _)| c)
            .collect();
        let code: String = chars
            .iter()
            .filter(|(_, l)| l.code)
            .map(|(c, _)| c)
            .collect();
        assert_eq!((bold.as_str(), code.as_str()), ("Done.", "cargo test"));
        assert_eq!(plain(&inline("a [b] c * d")), "a [b] c * d");
    }
}
