//! The line at the bottom: where you are on the left, what you can do or
//! what is running on the right.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{action::Action, component::RenderCtx};

pub struct StatusBar<'a> {
    pub location: Vec<Span<'a>>,
    /// Shown instead of the hints while something runs.
    pub activity: Option<Vec<Span<'a>>>,
    pub hints: &'a [(Action, &'a str)],
}

impl StatusBar<'_> {
    pub fn render(self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        f.render_widget(Paragraph::new("").style(theme.surface()), area);

        let right: Vec<Span> = match self.activity {
            Some(activity) => activity,
            None => self
                .hints
                .iter()
                .flat_map(|(action, label)| {
                    [
                        Span::styled(format!(" {} ", ctx.keymap.hint(*action)), theme.key()),
                        Span::styled(format!("{label} "), theme.dim()),
                    ]
                })
                .collect(),
        };
        let right = Line::from(right);
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right.width() as u16)])
                .areas(area);

        let mut location = vec![Span::raw(" ")];
        location.extend(self.location);
        f.render_widget(
            Paragraph::new(Line::from(location)).style(theme.surface()),
            left_area,
        );
        f.render_widget(Paragraph::new(right).style(theme.surface()), right_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn status_bar_with_hints() {
        let test = TestCtx::new();
        let snapshot = render(&test, 80, 1, |f, area, ctx| {
            StatusBar {
                location: vec![Span::raw("postgres@localhost:5432/shop")],
                activity: None,
                hints: &[(Action::Execute, "run"), (Action::Help, "help")],
            }
            .render(f, area, ctx)
        });
        insta::assert_snapshot!(snapshot, @" postgres@localhost:5432/shop                                    F5 run  ? help");
    }

    #[test]
    fn status_bar_while_running() {
        let test = TestCtx::new();
        let snapshot = render(&test, 80, 1, |f, area, ctx| {
            StatusBar {
                location: vec![Span::raw("shop")],
                activity: Some(vec![Span::raw("Running… 1.2 s")]),
                hints: &[(Action::Execute, "run")],
            }
            .render(f, area, ctx)
        });
        insta::assert_snapshot!(snapshot, @" shop                                                             Running… 1.2 s");
    }
}
