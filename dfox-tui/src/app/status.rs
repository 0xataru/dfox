//! The status bar's content for the current screen.

use dfox_core::result::ResultSet;
use ratatui::{style::Style, text::Span};

use super::{App, Screen};
use crate::{
    action::Action,
    component::RenderCtx,
    components::{manager::tag_color, results::format_duration, workspace::Panel},
    connections::Tag,
    widgets::{status_bar::StatusBar, throbber::spinner},
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
            .and_then(|w| w.results.running.as_ref().map(|q| (w, q)));
        let activity = running.map(|(workspace, query)| {
            vec![
                spinner(ctx, theme.key()),
                Span::raw(format!(
                    " {} · {} rows ",
                    format_duration(ctx.now.saturating_duration_since(query.started)),
                    workspace.results.store.row_count()
                )),
                Span::styled(
                    format!(" {} ", ctx.keymap.hint(Action::CopyRow)),
                    theme.key(),
                ),
                Span::styled("cancel ", theme.dim()),
            ]
        });

        let hints: &[(Action, &str)] = match self.screen {
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
                    (Action::NextFocus, "focus"),
                    (Action::Help, "keys"),
                ],
                Some(Panel::Explorer) => &[
                    (Action::Filter, "filter"),
                    (Action::Execute, "run"),
                    (Action::Menu, "menu"),
                    (Action::Help, "keys"),
                ],
                _ => &[
                    (Action::Execute, "run"),
                    (Action::NextFocus, "focus"),
                    (Action::Menu, "menu"),
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
