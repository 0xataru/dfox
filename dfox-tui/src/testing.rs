//! Rendering helpers for snapshot tests.

use std::collections::BTreeMap;

use ratatui::{backend::TestBackend, layout::Rect, Frame, Terminal};
use tokio::time::Instant;

use crate::{
    component::RenderCtx,
    icons::Icons,
    keymap::{Keymap, Preset},
    theme::Theme,
};

/// Fixed inputs for rendering, so snapshots don't change from run to run.
pub struct TestCtx {
    pub theme: Theme,
    pub icons: Icons,
    pub keymap: Keymap,
    pub now: Instant,
}

impl TestCtx {
    pub fn new() -> Self {
        Self {
            theme: Theme::default(),
            icons: Icons::unicode(),
            keymap: Keymap::new(Preset::Standard, &BTreeMap::new()).0,
            now: Instant::now(),
        }
    }

    pub fn render_ctx(&self) -> RenderCtx<'_> {
        RenderCtx {
            theme: &self.theme,
            icons: &self.icons,
            keymap: &self.keymap,
            now: self.now,
            frame: 0,
            animations: true,
        }
    }
}

/// Draws with `draw` on a `width`×`height` screen and returns the text, one
/// line per row, trailing spaces trimmed.
pub fn render(
    test: &TestCtx,
    width: u16,
    height: u16,
    draw: impl FnOnce(&mut Frame, Rect, &RenderCtx),
) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let ctx = test.render_ctx();
    terminal
        .draw(|f| {
            let area = f.area();
            draw(f, area, &ctx)
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    buffer
        .content
        .chunks(width as usize)
        .map(|row| {
            let line: String = row.iter().map(|cell| cell.symbol()).collect();
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything a handler needs, with the engine's commands kept for checking.
pub struct Harness {
    pub engine: dfox_core::engine::EngineHandle,
    pub commands: tokio::sync::mpsc::UnboundedReceiver<dfox_core::engine::Command>,
    pub config: crate::config::Config,
    pub toasts: crate::widgets::toast::Toasts,
    pub log: crate::component::EventLog,
    pub clipboard: crate::clipboard::Memory,
    pub history: crate::history::History,
    pub now: Instant,
}

impl Harness {
    pub fn new() -> Self {
        let (tx, commands) = tokio::sync::mpsc::unbounded_channel();
        Self {
            engine: dfox_core::engine::EngineHandle::new(tx),
            commands,
            config: crate::config::Config::default(),
            toasts: Default::default(),
            log: Default::default(),
            clipboard: Default::default(),
            history: Default::default(),
            now: Instant::now(),
        }
    }

    pub fn ctx(&mut self) -> crate::component::Ctx<'_> {
        crate::component::Ctx {
            engine: &self.engine,
            config: &self.config,
            toasts: &mut self.toasts,
            log: &mut self.log,
            clipboard: &mut self.clipboard,
            history: &mut self.history,
            now: self.now,
        }
    }

    /// The commands sent to the engine so far.
    pub fn sent(&mut self) -> Vec<dfox_core::engine::Command> {
        std::iter::from_fn(|| self.commands.try_recv().ok()).collect()
    }

    /// The text of the toasts shown so far.
    pub fn toast_texts(&self) -> Vec<String> {
        self.toasts.items().iter().map(|t| t.text.clone()).collect()
    }
}

/// Tables, views and routines for explorer tests and snapshots.
pub fn sample_objects() -> Vec<dfox_core::models::schema::DbObject> {
    use dfox_core::models::schema::{DbObject, ObjectKind};
    let object = |name: &str, kind, rows| DbObject {
        name: name.to_string(),
        kind,
        rows,
    };
    vec![
        object("active_customers", ObjectKind::View, None),
        object("customers", ObjectKind::Table, Some(1_250)),
        object("order_items", ObjectKind::Table, Some(48_200)),
        object("orders", ObjectKind::Table, Some(12_400)),
        object("place_order", ObjectKind::Procedure, None),
        object("total_spent", ObjectKind::Function, None),
    ]
}
