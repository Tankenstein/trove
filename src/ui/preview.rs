use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{App, agent_style, dim};
use crate::agents::{Agent, Role};
use crate::display::{ago, datetime, started, tilde};
use crate::index::row_message;
use crate::search::{Excerpt, Session};
use crate::text::{self, Chars};

const LABEL: usize = 8;
const MESSAGE_LINES: usize = 4;

impl App<'_> {
    fn preview_header(&self, s: &Session, width: usize, beside: bool) -> Vec<Line<'static>> {
        let gone = self.gone.get(&s.id).copied().unwrap_or(false);
        let folder_style = if gone {
            Style::new().fg(Color::Red)
        } else {
            dim()
        };
        let plural = if s.messages == 1 { "" } else { "s" };
        let mut about = vec![
            Span::styled(tilde(&s.cwd, &self.home), folder_style),
            Span::styled(" · ", dim()),
            Span::styled(s.agent.name(), agent_style(s.agent)),
        ];
        if let Some(model) = self.model(s) {
            about.push(Span::styled(format!(" · {model}"), dim()));
        }
        about.push(Span::styled(
            format!(" · {} message{plural}", s.messages),
            dim(),
        ));
        if s.archived {
            about.push(Span::styled(" · archived", dim()));
        }
        if gone {
            about.push(Span::styled(" · folder gone", Style::new().fg(Color::Red)));
        }
        let active = format!("active {}", ago(self.now - s.updated));
        if beside {
            let when = match datetime(s.started, &self.tz) {
                w if w.is_empty() => active,
                w => format!("started {w} · {active}"),
            };
            let title = text::cut(&text::marked(&s.title, &self.query.tokens), width);
            vec![
                Line::from(text::spans(
                    &title,
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::from(about),
                Line::styled(when, dim()),
                Line::raw(""),
            ]
        } else {
            let when = match started(s.started, self.now, &self.tz) {
                w if w.is_empty() => format!(" · {active}"),
                w => format!(" · started {w} · {active}"),
            };
            about.push(Span::styled(when, dim()));
            vec![Line::from(about), Line::raw("")]
        }
    }

    pub(super) fn preview_lines(
        &self,
        s: &Session,
        excerpts: &[Excerpt],
        matches: &[i64],
        width: usize,
        height: usize,
        beside: bool,
    ) -> Vec<Line<'static>> {
        let mut lines = self.preview_header(s, width, beside);
        let text_width = width.saturating_sub(LABEL).max(8);
        let bodies: Vec<Vec<Chars>> = excerpts
            .iter()
            .map(|e| text::markdown(&e.text, &self.query.tokens, text_width))
            .collect();
        let heights: Vec<usize> = bodies.iter().map(|b| b.len().max(1)).collect();
        let (required, optional) = priorities(excerpts, matches);
        let (shown, cap) = fit(
            &heights,
            &required,
            &optional,
            height.saturating_sub(lines.len()),
        );

        let mut previous = None;
        for (i, e) in excerpts.iter().enumerate().filter(|&(i, _)| shown[i]) {
            if let Some(p) = previous {
                let adjacent = row_message(e.row) == row_message(p) + 1;
                lines.push(if adjacent {
                    Line::raw("")
                } else {
                    Line::styled("  ⋯", dim())
                });
            }
            previous = Some(e.row);
            let label_style = if matches.contains(&e.row) {
                text::match_style(dim())
            } else {
                dim()
            };
            let body_style = match e.role {
                Role::User => Style::new(),
                Role::Assistant => dim(),
            };
            let body = &bodies[i];
            let take = body.len().min(cap);
            let label = speaker(e.role, s.agent);
            if body.is_empty() {
                lines.push(Line::styled(label.to_owned(), label_style));
            }
            for (n, line) in body.iter().take(take).enumerate() {
                let line = if n + 1 == take && take < body.len() {
                    text::continued(line, text_width)
                } else {
                    line.clone()
                };
                let label = if n == 0 { label } else { "" };
                let mut parts = vec![Span::styled(format!("{label:<LABEL$}"), label_style)];
                parts.extend(text::spans(&line, body_style));
                lines.push(Line::from(parts));
            }
        }
        lines
    }
}

fn speaker(role: Role, agent: &dyn Agent) -> &'static str {
    match role {
        Role::User => "you",
        Role::Assistant => agent.name(),
    }
}

fn priorities(excerpts: &[Excerpt], matches: &[i64]) -> (Vec<usize>, Vec<usize>) {
    let n = excerpts.len();
    let ends = [0, 1, n.saturating_sub(2), n.saturating_sub(1)];
    let near = |i: usize| {
        let at = row_message(excerpts[i].row);
        matches.iter().any(|&m| (at - row_message(m)).abs() <= 1)
    };
    let mut required: Vec<usize> = if matches.is_empty() {
        ends.into_iter().filter(|&i| i < n).collect()
    } else {
        (0..n).filter(|&i| near(i)).collect()
    };
    required.sort_unstable();
    required.dedup();
    let optional = [0, 1]
        .into_iter()
        .chain((0..n).rev())
        .filter(|&i| i < n && !required.contains(&i))
        .collect();
    (required, optional)
}

fn fit(
    heights: &[usize],
    required: &[usize],
    optional: &[usize],
    budget: usize,
) -> (Vec<bool>, usize) {
    let total = |shown: &[bool], cap: usize| {
        let chosen = heights.iter().zip(shown).filter(|&(_, &s)| s);
        let (lines, count) =
            chosen.fold((0usize, 0usize), |(l, c), (&h, _)| (l + h.min(cap), c + 1));
        lines + count.saturating_sub(1)
    };
    for cap in (1..=MESSAGE_LINES).rev() {
        let mut shown = vec![false; heights.len()];
        required.iter().for_each(|&i| shown[i] = true);
        if cap > 1 && total(&shown, cap) > budget {
            continue;
        }
        for &i in optional {
            shown[i] = true;
            if total(&shown, cap) > budget {
                shown[i] = false;
            }
        }
        return (shown, cap);
    }
    unreachable!("a cap of one line always returns")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting_messages() {
        assert_eq!(
            fit(&[2, 6, 1], &[0, 2], &[1], 20),
            (vec![true, true, true], 4)
        );
        assert_eq!(
            fit(&[4, 4, 4, 1], &[0, 3], &[1, 2], 11),
            (vec![true, true, false, true], 4)
        );
        assert_eq!(fit(&[6, 6], &[0, 1], &[], 7), (vec![true, true], 3));
    }
}
