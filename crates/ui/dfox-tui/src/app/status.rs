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
                format!("{} ", ctx.icons.engine(session.connection.driver)),
                theme.engine(session.connection.driver).bg(theme.surface),
            ));
            location.push(Span::styled(
                session.connection.name.as_str(),
                theme.base().bg(theme.surface),
            ));
            location.push(Span::styled(
                format!(" · {}", session.database),
                theme.dim(),
            ));
            if session.connection.read_only {
                location.push(Span::styled(" read-only", theme.warning()));
            }
            if self.workspace.as_ref().is_some_and(|w| w.in_transaction) {
                location.push(Span::raw(" "));
                location.push(Span::styled(
                    " TXN ",
                    Style::default()
                        .fg(theme.bg)
                        .bg(theme.warning)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ));
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
        // An export runs in the background, on any screen.
        let exporting = self.workspace.as_ref().and_then(|w| w.exporting());
        let activity = activity.or_else(|| {
            let export = exporting?;
            let name = export
                .file
                .file_name()
                .map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let mut spans = vec![spinner(ctx, theme.key())];
            match (&export.import, &export.copy_to) {
                // Rows travel from the source to the target connection.
                (_, Some(into)) => {
                    spans.push(Span::raw(format!(" copying {name} ")));
                    spans.extend(travel(ctx));
                    spans.push(Span::raw(format!(" {into}")));
                }
                (Some(_), None) => spans.push(Span::raw(format!(" importing {name}"))),
                (None, None) => spans.push(Span::raw(format!(" exporting {name}"))),
            }
            let estimate = export.estimate.filter(|e| *e > 0);
            if let Some(estimate) = estimate {
                let ratio = (export.rows as f64 / estimate as f64).min(0.99);
                spans.push(Span::raw(" "));
                spans.extend(cells_bar(ratio, ctx));
                spans.push(Span::raw(format!(" {}%", (ratio * 100.) as u64)));
            }
            let mut text = format!(" · {}", count(export.rows as usize));
            let took = ctx.now.saturating_duration_since(export.started);
            // A rate once there is enough to go on.
            if took >= Duration::from_millis(500) && export.rows > 0 {
                let rate = export.rows as f64 / took.as_secs_f64();
                text.push_str(&format!(" · {}/s", per_second(rate)));
                if let Some(estimate) = estimate.filter(|e| *e > export.rows) {
                    let left = (estimate - export.rows) as f64 / rate;
                    text.push_str(&format!(" · {} left", time_left(left)));
                }
            }
            spans.push(Span::raw(format!("{text} ")));
            spans.push(Span::styled(
                format!(" {} ", ctx.keymap.hint(Action::Copy)),
                theme.key(),
            ));
            spans.push(Span::styled("cancel ", theme.dim()));
            Some(spans)
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
                    (Action::JsonView, "json"),
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
/// A bar of ten cells, filled to `ratio`, the leading cell lit.
fn cells_bar(ratio: f64, ctx: &RenderCtx) -> Vec<Span<'static>> {
    const CELLS: usize = 10;
    let filled = ((ratio * CELLS as f64).round() as usize).min(CELLS);
    (0..CELLS)
        .map(|i| match i {
            i if i + 1 == filled => Span::styled("▰", ctx.theme.key()),
            i if i < filled => Span::styled("▰", ctx.theme.dim()),
            _ => Span::styled("▱", ctx.theme.border(false)),
        })
        .collect()
}

/// Rows moving between two connections: a few cells marching right.
fn travel(ctx: &RenderCtx) -> Vec<Span<'static>> {
    const SLOTS: u64 = 7;
    let lead = if ctx.animations { ctx.frame % SLOTS } else { 0 };
    (0..SLOTS)
        .map(|i| {
            if (i + SLOTS - lead).is_multiple_of(3) {
                Span::styled("▪", ctx.theme.key())
            } else {
                Span::styled("·", ctx.theme.dim())
            }
        })
        .chain(std::iter::once(Span::styled("›", ctx.theme.key())))
        .collect()
}

/// `1.2k rows`, `340 rows`, `2.1M rows`.
fn per_second(rate: f64) -> String {
    match rate {
        r if r >= 1e6 => format!("{:.1}M rows", r / 1e6),
        r if r >= 1e3 => format!("{:.1}k rows", r / 1e3),
        r => format!("{r:.0} rows"),
    }
}

/// `4 s`, `2 min`.
fn time_left(seconds: f64) -> String {
    if seconds < 90. {
        format!("{} s", seconds.ceil().max(1.) as u64)
    } else {
        format!("{} min", (seconds / 60.).round() as u64)
    }
}

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
    use super::{per_second, time_left};

    #[test]
    fn rates_and_time_left_read_short() {
        assert_eq!(per_second(340.4), "340 rows");
        assert_eq!(per_second(12_345.), "12.3k rows");
        assert_eq!(per_second(2_100_000.), "2.1M rows");
        assert_eq!(time_left(0.2), "1 s");
        assert_eq!(time_left(41.3), "42 s");
        assert_eq!(time_left(600.), "10 min");
    }

    #[test]
    fn the_bar_fills_and_rows_travel() {
        let test = crate::testing::TestCtx::new();
        let mut ctx = test.render_ctx();
        let bar: String = super::cells_bar(0.62, &ctx)
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(bar, "▰▰▰▰▰▰▱▱▱▱");
        let line = |ctx: &crate::component::RenderCtx| -> String {
            super::travel(ctx)
                .iter()
                .map(|s| s.content.to_string())
                .collect()
        };
        let first = line(&ctx);
        ctx.frame += 1;
        assert_ne!(line(&ctx), first);
        assert!(first.ends_with('›'));
    }

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
