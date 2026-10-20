//! The status bar's content for the current screen.

use dfox_core::result::ResultSet;
use ratatui::{style::Style, text::Span};

use std::time::Duration;

use super::{App, Screen};
use crate::{
    action::Action,
    component::RenderCtx,
    components::{manager::tag_color, results::format_duration, workspace::Panel},
    connections::Tag,
    theme::Theme,
    widgets::{status_bar::StatusBar, throbber::spinner},
};

/// The version the manager shows. Fixed in tests, so screen snapshots don't
/// change with every release.
pub(super) const VERSION: &str = if cfg!(test) {
    "0.0.0"
} else {
    env!("CARGO_PKG_VERSION")
};

impl App {
    pub(super) fn status_bar(&self, ctx: &RenderCtx) -> StatusBar<'_> {
        let theme = ctx.theme;
        let mut location = vec![Span::styled("dfox", theme.key())];
        if let Some(connecting) = &self.connecting {
            location.push(Span::raw("  "));
            location.push(spinner(ctx, theme.key()));
            let target = connecting
                .switch_to
                .clone()
                .unwrap_or_else(|| connecting.connection.name.clone());
            location.push(Span::styled(
                format!(" connecting to {target}…"),
                theme.dim(),
            ));
        } else if self.screen == Screen::Manager && self.session.is_none() {
            location.push(Span::styled(format!(" {VERSION}"), theme.dim()));
            if let Some(update) = &self.update {
                location.push(Span::styled(format!("  ↑ {update}"), theme.key()));
            }
        } else if let Some(session) = &self.session {
            location.push(Span::raw("  "));
            if session.connection.tag != Tag::None {
                location.push(Span::styled(
                    "▌",
                    Style::default().fg(tag_color(theme, session.connection.tag)),
                ));
            }
            location.push(Span::styled(
                format!("{} {}", ctx.icons.database, session.connection.name),
                theme.base().bg(theme.surface),
            ));
            location.push(Span::styled(
                format!(" · {}", session.database),
                theme.dim(),
            ));
            if session.connection.read_only {
                location.push(Span::styled(" read-only", theme.warning()));
            }
        }

        let running = self
            .workspace
            .as_ref()
            .filter(|_| self.screen == Screen::Workspace)
            .and_then(|w| w.running())
            .and_then(|results| results.running.as_ref().map(|q| (results, q)));
        let activity = running.map(|(results, query)| {
            let elapsed = ctx.now.saturating_duration_since(query.started);
            let rows = results.store.row_count();
            let mut spans = vec![
                spinner(ctx, theme.key()),
                Span::raw(format!(" {} · {} ", format_duration(elapsed), count(rows))),
            ];
            if let Some(rate) = rate(rows, elapsed) {
                spans.push(Span::styled(format!("· {rate} "), theme.dim()));
            }
            // The cancel hint fades in once the query has taken a while.
            if let Some(alpha) = hint_alpha(elapsed, ctx.animations) {
                let key = Theme::blend(theme.bg, theme.accent, alpha);
                let text = Theme::blend(theme.bg, theme.text_dim, alpha);
                spans.push(Span::styled(
                    format!(" {} ", ctx.keymap.hint(Action::Copy)),
                    Style::default().fg(key),
                ));
                spans.push(Span::styled("cancel ", Style::default().fg(text)));
            }
            spans
        });

        let hints: &[(Action, &str)] = match self.screen {
            Screen::Splash => &[],
            Screen::Manager => &[
                (Action::Confirm, "connect"),
                (Action::New, "new"),
                (Action::Edit, "edit"),
                (Action::Help, "keys"),
            ],
            Screen::Form => &[(Action::Save, "save"), (Action::Back, "cancel")],
            Screen::Workspace => match self.workspace.as_ref().map(|w| w.focused()) {
                Some(Panel::Editor) => &[
                    (Action::Execute, "run"),
                    (Action::Palette, "commands"),
                    (Action::Connections, "connections"),
                    (Action::Help, "keys"),
                ],
                Some(Panel::Explorer) => &[
                    (Action::Confirm, "open"),
                    (Action::Inspect, "insert"),
                    (Action::Filter, "filter"),
                    (Action::Connections, "connections"),
                    (Action::Help, "keys"),
                ],
                _ => &[
                    (Action::Inspect, "inspect"),
                    (Action::CopyAs, "copy as"),
                    (Action::Connections, "connections"),
                    (Action::Help, "keys"),
                ],
            },
        };
        StatusBar {
            location,
            activity,
            hints,
        }
    }
}

/// The cancel hint shows after this long...
const HINT_AFTER: Duration = Duration::from_secs(2);
/// ...fading in over this long.
const HINT_FADE: Duration = Duration::from_millis(300);

/// How visible the cancel hint is after `elapsed`: hidden at first, then
/// fading in. Without animations it appears at once.
fn hint_alpha(elapsed: Duration, animations: bool) -> Option<f64> {
    let shown = elapsed.checked_sub(HINT_AFTER)?;
    if !animations {
        return Some(1.0);
    }
    Some((shown.as_secs_f64() / HINT_FADE.as_secs_f64()).min(1.0))
}

/// `1 row`, `48,213 rows`.
fn count(rows: usize) -> String {
    let n = crate::components::results::grid::group_digits(&rows.to_string());
    if rows == 1 {
        format!("{n} row")
    } else {
        format!("{n} rows")
    }
}

/// `23k rows/s` while rows stream in; nothing until there is a rate worth
/// showing.
fn rate(rows: usize, elapsed: Duration) -> Option<String> {
    let seconds = elapsed.as_secs_f64();
    if rows < 1000 || seconds < 0.2 {
        return None;
    }
    let per_second = rows as f64 / seconds;
    Some(if per_second >= 1_000_000.0 {
        format!("{:.1}M rows/s", per_second / 1_000_000.0)
    } else if per_second >= 1000.0 {
        format!("{:.0}k rows/s", per_second / 1000.0)
    } else {
        format!("{per_second:.0} rows/s")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cancel_hint_fades_in_after_two_seconds() {
        assert_eq!(hint_alpha(Duration::from_millis(1999), true), None);
        assert_eq!(hint_alpha(Duration::from_secs(2), true), Some(0.0));
        assert_eq!(hint_alpha(Duration::from_millis(2150), true), Some(0.5));
        assert_eq!(hint_alpha(Duration::from_secs(5), true), Some(1.0));
        assert_eq!(hint_alpha(Duration::from_millis(2001), false), Some(1.0));
    }

    #[test]
    fn rows_and_rates_read_naturally() {
        assert_eq!(count(1), "1 row");
        assert_eq!(count(48_213), "48,213 rows");
        assert_eq!(rate(500, Duration::from_secs(1)), None);
        assert_eq!(rate(5000, Duration::from_millis(100)), None);
        assert_eq!(
            rate(46_000, Duration::from_secs(2)).as_deref(),
            Some("23k rows/s")
        );
        assert_eq!(
            rate(3_000_000, Duration::from_secs(1)).as_deref(),
            Some("3.0M rows/s")
        );
        assert_eq!(
            rate(1500, Duration::from_secs(3)).as_deref(),
            Some("500 rows/s")
        );
    }
}
