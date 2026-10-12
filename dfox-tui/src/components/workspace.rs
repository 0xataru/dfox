//! The main screen once a database is open: the explorer, the query editor and
//! results. Tab moves focus between them.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    Frame,
};

use super::{editor::EditorPanel, explorer::Explorer, results::ResultsPanel};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, Focus, RenderCtx},
    widgets::toast::Level,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Explorer,
    Editor,
    Results,
}

const ORDER: &[Panel] = &[Panel::Explorer, Panel::Editor, Panel::Results];

#[derive(Debug)]
pub struct Workspace {
    focus: Focus<Panel>,
    pub explorer: Explorer,
    pub editor: EditorPanel,
    pub results: ResultsPanel,
}

impl Workspace {
    /// A workspace on `database`, the one the session is connected to.
    pub fn new(database: &str) -> Self {
        let mut workspace = Self {
            focus: Focus::new(ORDER),
            explorer: Explorer::new(database),
            editor: EditorPanel::default(),
            results: ResultsPanel::default(),
        };
        workspace.set_focus(Panel::Editor);
        workspace
    }

    pub fn focused(&self) -> Panel {
        self.focus.get()
    }

    fn set_focus(&mut self, panel: Panel) {
        self.focus.set(panel);
        self.sync_focus();
    }

    fn sync_focus(&mut self) {
        self.explorer.focused = self.focus.is(Panel::Explorer);
        self.editor.focused = self.focus.is(Panel::Editor);
        self.results.focused = self.focus.is(Panel::Results);
    }

    fn copy(&mut self, all: bool, ctx: &mut Ctx) {
        if self.results.running.is_some() {
            self.results.cancel(ctx);
        } else if !self.focus.is(Panel::Results) {
            ctx.toast(Level::Info, "Focus the results (Tab) to copy them");
        } else if all {
            self.results.copy_all(ctx);
        } else {
            self.results.copy_row(ctx);
        }
    }
}

impl Component for Workspace {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        match action {
            Action::NextFocus => {
                self.focus.next();
                self.sync_focus();
            }
            Action::PrevFocus => {
                self.focus.prev();
                self.sync_focus();
            }
            Action::Execute => self.results.run(&self.editor.text(), ctx),
            Action::CopyRow => self.copy(false, ctx),
            Action::CopyAll => self.copy(true, ctx),
            Action::Debug => {
                self.results.show_log(ctx.log.entries());
                self.set_focus(Panel::Results);
            }
            Action::Connections => {
                self.results.abandon(ctx);
                return Some(Command::Connections);
            }
            Action::Back => return Some(Command::ConfirmQuit),
            action => {
                return match self.focus.get() {
                    Panel::Explorer => self.explorer.handle(action, ctx),
                    Panel::Editor => self.editor.handle(action, ctx),
                    Panel::Results => self.results.handle(action, ctx),
                }
            }
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(28), Constraint::Fill(1)]).areas(area);
        let [editor, results] =
            Layout::vertical([Constraint::Percentage(35), Constraint::Fill(1)]).areas(right);
        self.explorer.render(f, left, ctx);
        self.editor.render(f, editor, ctx);
        self.results.render(f, results, ctx);
    }

    fn is_animating(&self) -> bool {
        self.explorer.is_animating() || self.results.is_animating()
    }

    fn wants_text(&self) -> bool {
        match self.focus.get() {
            Panel::Editor => true,
            Panel::Explorer => self.explorer.wants_text(),
            Panel::Results => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, Harness, TestCtx};
    use dfox_core::{engine::Command as DbCommand, result::ResultSet};

    #[test]
    fn tab_cycles_focus_and_text_goes_to_the_editor() {
        let mut harness = Harness::new();
        let mut workspace = Workspace::new("shop");
        assert!(workspace.wants_text());

        workspace.handle(Action::Input('x'), &mut harness.ctx());
        assert_eq!(workspace.editor.text(), "x");

        workspace.handle(Action::NextFocus, &mut harness.ctx());
        assert_eq!(workspace.focused(), Panel::Results);
        assert!(!workspace.wants_text());
        workspace.handle(Action::PrevFocus, &mut harness.ctx());
        workspace.handle(Action::PrevFocus, &mut harness.ctx());
        assert_eq!(workspace.focused(), Panel::Explorer);
    }

    #[test]
    fn copy_cancels_a_running_query() {
        let mut harness = Harness::new();
        let mut workspace = Workspace::new("shop");
        workspace.editor.set_text("SELECT pg_sleep(5)");
        workspace.handle(Action::Execute, &mut harness.ctx());
        harness.sent();

        workspace.handle(Action::CopyRow, &mut harness.ctx());
        assert!(matches!(&harness.sent()[..], [DbCommand::Cancel { .. }]));
    }

    #[test]
    fn copy_asks_for_the_results_to_be_focused() {
        let mut harness = Harness::new();
        let mut workspace = Workspace::new("shop");
        workspace.handle(Action::CopyAll, &mut harness.ctx());
        assert_eq!(
            harness.toast_texts(),
            ["Focus the results (Tab) to copy them"]
        );
    }

    #[test]
    fn leaving_abandons_the_query() {
        let mut harness = Harness::new();
        let mut workspace = Workspace::new("shop");
        workspace.editor.set_text("SELECT 1");
        workspace.handle(Action::Execute, &mut harness.ctx());

        let command = workspace.handle(Action::Connections, &mut harness.ctx());
        assert_eq!(command, Some(Command::Connections));
        assert!(workspace.results.running.is_none());
        assert_eq!(
            workspace.handle(Action::Back, &mut harness.ctx()),
            Some(Command::ConfirmQuit)
        );
    }

    #[test]
    fn the_event_log_shows_in_the_results() {
        let mut harness = Harness::new();
        harness.log.push("connected".into());
        let mut workspace = Workspace::new("shop");
        workspace.handle(Action::Debug, &mut harness.ctx());
        assert_eq!(workspace.results.store.columns()[1].name, "Event");
        assert_eq!(workspace.focused(), Panel::Results);
    }

    #[test]
    fn workspace_wide() {
        let test = TestCtx::new();
        let mut workspace = Workspace::new("shop");
        let mut harness = Harness::new();
        workspace.explorer.on_databases(
            Ok(vec!["postgres".into(), "shop".into()]),
            &mut harness.ctx(),
        );
        workspace
            .explorer
            .on_schemas(Ok(vec!["public".into()]), &mut harness.ctx());
        workspace
            .explorer
            .on_objects("public", Ok(crate::testing::sample_objects()));
        workspace.editor.set_text("SELECT * FROM customers");
        insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| workspace
            .render(f, area, ctx)));
    }

    #[test]
    fn workspace_narrow() {
        let test = TestCtx::new();
        let mut workspace = Workspace::new("shop");
        workspace.explorer.load(&mut Harness::new().ctx());
        insta::assert_snapshot!(
            render(&test, 80, 24, |f, area, ctx| workspace.render(f, area, ctx))
        );
    }
}
