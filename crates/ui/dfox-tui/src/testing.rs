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
pub fn sample_objects() -> Vec<dfox_core::catalog::Node> {
    use dfox_core::catalog::{Node, NodeKind};
    let object = |name: &str, kind: NodeKind, rows: Option<&str>| {
        let node = Node::new(kind, name, kind.has_rows());
        match rows {
            Some(rows) => node.with_detail(rows),
            None => node,
        }
    };
    vec![
        object("active_customers", NodeKind::View, None),
        object("customers", NodeKind::Table, Some("~1.2k")),
        object("order_items", NodeKind::Table, Some("~48.2k")),
        object("orders", NodeKind::Table, Some("~12.4k")),
        object("place_order", NodeKind::Procedure, None),
        object("total_spent", NodeKind::Function, None),
    ]
}

/// Two schemas with tables and columns, for autocomplete tests.
pub fn sample_catalog() -> crate::catalog::Catalog {
    let mut catalog = crate::catalog::Catalog::default();
    catalog.on_schemas(&["public".into(), "audit".into()]);
    catalog.on_tables("audit", vec!["log".into(), "orders".into()]);
    catalog.on_tables("public", vec!["customers".into(), "orders".into()]);
    let column = |table: &str, name: &str| dfox_core::models::schema::TableColumn {
        table: table.into(),
        name: name.into(),
        data_type: "int".into(),
    };
    catalog.on_columns(
        "public",
        Ok(vec![
            column("customers", "id"),
            column("customers", "name"),
            column("orders", "id"),
            column("orders", "customer_id"),
        ]),
    );
    catalog.on_columns("audit", Ok(vec![column("orders", "at")]));
    catalog
}

/// The schema and batch a driver would send for `rows`: what the engine
/// passes on as a query's `Schema` and `Batch` events.
pub fn frames(
    columns: Vec<dfox_core::result::ColumnMeta>,
    rows: Vec<Vec<dfox_core::result::OwnedCell>>,
) -> (arrow_schema::SchemaRef, arrow_array::RecordBatch) {
    let result = dfox_core::arrow::ArrowResultSet::from_cells(columns, rows);
    let schema = result.schema().clone();
    let batch = result
        .batches()
        .first()
        .cloned()
        .unwrap_or_else(|| arrow_array::RecordBatch::new_empty(schema.clone()));
    (schema, batch)
}

/// One column `n` of integers, as a query's schema and batch.
pub fn ints(values: &[i64]) -> (arrow_schema::SchemaRef, arrow_array::RecordBatch) {
    use dfox_core::result::{ColumnMeta, OwnedCell};
    frames(
        vec![ColumnMeta::new("n", "int4")],
        values.iter().map(|v| vec![OwnedCell::Int(*v)]).collect(),
    )
}

/// Rows of cells read through [`ResultSet`](dfox_core::result::ResultSet),
/// for tests of code that takes any result: a column may mix kinds of value,
/// as no Arrow result can.
#[derive(Debug, Default)]
pub struct Cells {
    pub columns: Vec<dfox_core::result::ColumnMeta>,
    pub rows: Vec<Vec<dfox_core::result::OwnedCell>>,
}

impl Cells {
    pub fn new(columns: Vec<dfox_core::result::ColumnMeta>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Vec<dfox_core::result::OwnedCell>) {
        self.rows.push(row);
    }
}

impl dfox_core::result::ResultSet for Cells {
    fn columns(&self) -> &[dfox_core::result::ColumnMeta] {
        &self.columns
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, col: usize) -> Option<dfox_core::result::Cell<'_>> {
        self.rows.get(row)?.get(col).map(|c| c.as_cell())
    }

    fn is_complete(&self) -> bool {
        true
    }
}
