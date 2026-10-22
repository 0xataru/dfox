//! Editing a table's rows in the grid: changed cells, new rows and deleted
//! rows wait here until they are saved together, as one change set the
//! driver writes in a transaction. Rows are found by the table's primary
//! key, which the result must hold. New rows come after the result's own;
//! the columns they leave empty get their defaults.

use dfox_core::{
    catalog::NodePath,
    result::{Cell, OwnedCell, ResultSet},
    session::{Change, ChangeSet},
};

use super::grid::Marks;

/// One step to undo: a cell as it was before, or a row marked deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// The cell's earlier pending value; `None` when it had none.
    Cell((usize, usize), Option<Option<String>>),
    Delete(usize),
    Add,
}

/// The edits of one result, made to the table at `table`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edits {
    pub table: NodePath,
    /// The primary key's columns, and where the result holds them.
    key: Vec<(String, usize)>,
    pub marks: Marks,
    /// The result's own rows; new ones follow.
    rows: usize,
    /// New rows.
    pub added: usize,
    undo: Vec<Step>,
    /// The edits went to the database and the answer hasn't come.
    pub saving: bool,
}

impl Edits {
    /// Edits of `result`, rows of `table` keyed by the columns `key`. Fails
    /// when the result doesn't hold every key column.
    pub fn new(table: NodePath, key: &[String], result: &impl ResultSet) -> Result<Self, String> {
        let key = key
            .iter()
            .map(|name| {
                result
                    .columns()
                    .iter()
                    .position(|c| &c.name == name)
                    .map(|i| (name.clone(), i))
                    .ok_or_else(|| format!("the result has no {name} column, the table's key"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            table,
            key,
            marks: Marks::default(),
            rows: result.row_count(),
            added: 0,
            undo: Vec::new(),
            saving: false,
        })
    }

    /// How many changes there are: cells of rows kept, rows deleted, and
    /// new rows.
    pub fn count(&self) -> usize {
        let kept = |row: &usize| !self.marks.deleted.contains(row);
        let cells = self
            .marks
            .cells
            .keys()
            .filter(|(row, _)| *row < self.rows && kept(row))
            .count();
        let deleted = self
            .marks
            .deleted
            .iter()
            .filter(|r| **r < self.rows)
            .count();
        let added = (self.rows..self.rows + self.added).filter(kept).count();
        cells + deleted + added
    }

    /// True when the cell at `row`, `col` is part of an existing row's key,
    /// which finds the row and can't change here.
    pub fn is_key(&self, row: usize, col: usize) -> bool {
        row < self.rows && self.key.iter().any(|(_, i)| *i == col)
    }

    /// Adds a row after the others, and returns where it is.
    pub fn add_row(&mut self) -> usize {
        self.added += 1;
        self.undo.push(Step::Add);
        self.rows + self.added - 1
    }

    /// Sets the cell at `row`, `col` to `value` (`None` for NULL). Setting
    /// it back to what the result holds drops the change.
    pub fn set(&mut self, result: &impl ResultSet, row: usize, col: usize, value: Option<String>) {
        let before = self.marks.cells.get(&(row, col)).cloned();
        let original = match result.cell(row, col) {
            None | Some(Cell::Null) => None,
            Some(cell) => Some(cell.to_string()),
        };
        if value == original {
            self.marks.cells.remove(&(row, col));
        } else {
            self.marks.cells.insert((row, col), value);
        }
        if self.marks.cells.get(&(row, col)) != before.as_ref() {
            self.undo.push(Step::Cell((row, col), before));
        }
    }

    /// Marks `row` deleted, or not any more.
    pub fn toggle_delete(&mut self, row: usize) {
        if !self.marks.deleted.remove(&row) {
            self.marks.deleted.insert(row);
        }
        self.undo.push(Step::Delete(row));
    }

    /// Undoes the last change. Returns false when there was none.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(Step::Cell(at, Some(before))) => {
                self.marks.cells.insert(at, before);
            }
            Some(Step::Cell(at, None)) => {
                self.marks.cells.remove(&at);
            }
            Some(Step::Delete(row)) => {
                if !self.marks.deleted.remove(&row) {
                    self.marks.deleted.insert(row);
                }
            }
            Some(Step::Add) => {
                self.added -= 1;
                let row = self.rows + self.added;
                self.marks.cells.retain(|(r, _), _| *r != row);
                self.marks.deleted.remove(&row);
            }
            None => return false,
        }
        true
    }

    /// The key of `row`, as values.
    fn key_of(&self, result: &impl ResultSet, row: usize) -> Vec<OwnedCell> {
        self.key
            .iter()
            .map(|(_, col)| result.cell(row, *col).map_or(OwnedCell::Null, owned))
            .collect()
    }

    /// `name = value` for the key of `row`: how the review names it.
    fn key_text(&self, result: &impl ResultSet, row: usize) -> String {
        self.key
            .iter()
            .map(|(name, col)| {
                let shown = result
                    .cell(row, *col)
                    .map_or("NULL".to_string(), |c| shown(&c));
                format!("{name} = {shown}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The cells set in `row`, by column name.
    fn values(&self, result: &impl ResultSet, row: usize) -> Vec<(String, OwnedCell)> {
        self.marks
            .cells
            .range((row, 0)..(row, usize::MAX))
            .map(|((_, col), value)| {
                let name = result.columns()[*col].name.clone();
                let value = value
                    .as_ref()
                    .map_or(OwnedCell::Null, |v| OwnedCell::Text(v.as_str().into()));
                (name, value)
            })
            .collect()
    }

    /// The edits as the driver writes them: an update per changed row not
    /// deleted, a delete per deleted row, an insert per new row.
    pub fn change_set(&self, result: &impl ResultSet) -> ChangeSet {
        let mut changes = Vec::new();
        let mut rows: Vec<usize> = self.marks.cells.keys().map(|(row, _)| *row).collect();
        rows.dedup();
        let new = |row: usize| row >= self.rows;
        for row in rows
            .into_iter()
            .filter(|r| !new(*r) && !self.marks.deleted.contains(r))
        {
            changes.push(Change::Update {
                key: self.key_of(result, row),
                set: self.values(result, row),
            });
        }
        for row in self.marks.deleted.iter().filter(|r| !new(**r)) {
            changes.push(Change::Delete(self.key_of(result, *row)));
        }
        for row in self.rows..self.rows + self.added {
            if !self.marks.deleted.contains(&row) {
                changes.push(Change::Insert(self.values(result, row)));
            }
        }
        ChangeSet {
            table: self.table.clone(),
            key: self.key.iter().map(|(name, _)| name.clone()).collect(),
            changes,
        }
    }

    /// The edits in words, a line each, for the review before saving.
    pub fn review(&self, result: &impl ResultSet) -> Vec<String> {
        let mut lines = Vec::new();
        for (&(row, col), value) in &self.marks.cells {
            if row >= self.rows || self.marks.deleted.contains(&row) {
                continue;
            }
            let before = result
                .cell(row, col)
                .map_or("NULL".to_string(), |c| shown(&c));
            let after = value
                .as_deref()
                .map_or("NULL".to_string(), |v| format!("{v:?}"));
            lines.push(format!(
                "{}: {} {before} → {after}",
                self.key_text(result, row),
                result.columns()[col].name
            ));
        }
        for &row in self.marks.deleted.iter().filter(|r| **r < self.rows) {
            lines.push(format!("{}: delete the row", self.key_text(result, row)));
        }
        for row in self.rows..self.rows + self.added {
            if self.marks.deleted.contains(&row) {
                continue;
            }
            let values: Vec<String> = self
                .values(result, row)
                .into_iter()
                .map(|(name, value)| match value {
                    OwnedCell::Text(text) => format!("{name} {text:?}"),
                    _ => format!("{name} NULL"),
                })
                .collect();
            lines.push(if values.is_empty() {
                "new row: every column its default".to_string()
            } else {
                format!("new row: {}", values.join(", "))
            });
        }
        lines
    }
}

/// A value as the review shows it: text quoted, NULL as NULL.
fn shown(cell: &Cell<'_>) -> String {
    match cell {
        Cell::Null => "NULL".to_string(),
        Cell::Text(text) => format!("{text:?}"),
        other => other.to_string(),
    }
}

/// A value of the result, kept: the key of a row.
fn owned(cell: Cell<'_>) -> OwnedCell {
    match cell {
        Cell::Null => OwnedCell::Null,
        Cell::Bool(b) => OwnedCell::Bool(b),
        Cell::Int(n) => OwnedCell::Int(n),
        Cell::Float(n) => OwnedCell::Float(n),
        // Exact or of a kind the driver casts from text.
        other => OwnedCell::Text(other.to_string().into()),
    }
}

#[cfg(test)]
mod tests {
    use dfox_core::{arrow::ArrowResultSet, result::ColumnMeta};

    use super::*;

    fn users() -> ArrowResultSet {
        ArrowResultSet::from_cells(
            vec![
                ColumnMeta::new("id", "int4"),
                ColumnMeta::new("name", "text"),
                ColumnMeta::new("note", "text"),
            ],
            vec![
                vec![
                    OwnedCell::Int(1),
                    OwnedCell::Text("Ann".into()),
                    OwnedCell::Null,
                ],
                vec![
                    OwnedCell::Int(2),
                    OwnedCell::Text("Bob".into()),
                    OwnedCell::Null,
                ],
            ],
        )
    }

    fn edits(result: &ArrowResultSet) -> Edits {
        Edits::new(
            ["shop", "public", "users"].into_iter().collect(),
            &["id".into()],
            result,
        )
        .expect("the key is there")
    }

    #[test]
    fn changes_become_updates_and_deletes_by_key() {
        let result = users();
        let mut e = edits(&result);
        e.set(&result, 0, 1, Some("Anna".into()));
        e.set(&result, 0, 2, None);
        e.set(&result, 1, 2, Some("vip".into()));
        e.toggle_delete(1);
        // Setting a NULL to NULL changes nothing.
        assert_eq!(e.count(), 2);
        let set = e.change_set(&result);
        assert_eq!(set.key, ["id"]);
        assert_eq!(
            set.changes,
            [
                Change::Update {
                    key: vec![OwnedCell::Int(1)],
                    set: vec![("name".into(), OwnedCell::Text("Anna".into()))],
                },
                Change::Delete(vec![OwnedCell::Int(2)]),
            ]
        );
        assert_eq!(
            e.review(&result),
            ["id = 1: name \"Ann\" → \"Anna\"", "id = 2: delete the row"]
        );
    }

    #[test]
    fn setting_a_cell_back_and_undoing_drop_changes() {
        let result = users();
        let mut e = edits(&result);
        e.set(&result, 0, 1, Some("Anna".into()));
        e.set(&result, 0, 1, Some("Ann".into()));
        assert_eq!(e.count(), 0, "back to what the table holds");
        e.set(&result, 0, 1, Some("A".into()));
        e.set(&result, 0, 1, Some("An".into()));
        e.toggle_delete(1);
        assert!(e.undo());
        assert!(e.marks.deleted.is_empty());
        assert!(e.undo());
        assert_eq!(e.marks.cells.get(&(0, 1)), Some(&Some("A".into())));
        assert!(e.undo());
        assert!(e.marks.cells.is_empty());
        // Further back: "Anna", then nothing.
        assert!(e.undo());
        assert_eq!(e.marks.cells.get(&(0, 1)), Some(&Some("Anna".into())));
        assert!(e.undo());
        assert!(!e.undo());
        assert_eq!(e.count(), 0);
    }

    #[test]
    fn new_rows_insert_what_is_set_and_defaults_the_rest() {
        let result = users();
        let mut e = edits(&result);
        assert_eq!(e.add_row(), 2);
        e.set(&result, 2, 1, Some("Cy".into()));
        // A new row's key can be given; an existing row's can't change.
        assert!(!e.is_key(2, 0));
        assert!(e.is_key(0, 0));
        assert_eq!(e.add_row(), 3);
        let dropped = e.add_row();
        e.toggle_delete(dropped);
        assert_eq!(e.count(), 2, "two new rows kept");
        let set = e.change_set(&result);
        assert_eq!(
            set.changes,
            [
                Change::Insert(vec![("name".into(), OwnedCell::Text("Cy".into()))]),
                Change::Insert(Vec::new()),
            ]
        );
        assert_eq!(
            e.review(&result),
            ["new row: name \"Cy\"", "new row: every column its default"]
        );
        // Undo the delete, then that row, then the next.
        assert!(e.undo());
        assert!(e.undo());
        assert_eq!(e.added, 2);
        assert!(e.undo());
        assert_eq!(e.added, 1);
    }

    #[test]
    fn the_key_must_be_in_the_result() {
        let result = users();
        let err = Edits::new(NodePath::root(), &["uuid".into()], &result).unwrap_err();
        assert!(err.contains("uuid"), "{err}");
        assert!(edits(&result).is_key(0, 0));
        assert!(!edits(&result).is_key(0, 1));
    }
}
