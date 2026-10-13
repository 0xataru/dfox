//! Reading a [`ResultStream`] into an [`ArrowResultSet`] within a memory
//! budget, so a huge result can't take all of memory.

use futures_util::StreamExt;

use super::{ArrowResultSet, Frame, ResultStream};
use crate::errors::DbError;

/// How much a result may hold before fetching stops: 256 MB.
pub const DEFAULT_BUDGET: usize = 256 * 1024 * 1024;

/// What happens when a result outgrows its budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Stop fetching; the user can ask for more.
    #[default]
    Stop,
    /// Write the batches to a temporary Arrow IPC file and keep going. Not
    /// built yet: fetching fails when the budget is reached.
    Spill,
}

/// Where fetching stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStatus {
    /// Every statement finished.
    Done,
    /// The result reached its budget; [`Fetcher::fetch_more`] continues.
    OverBudget,
}

/// Pulls a run's frames into a result. The last result set wins, like a
/// script's last `SELECT`.
pub struct Fetcher {
    stream: Option<ResultStream>,
    budget: usize,
    overflow: Overflow,
    result: Option<ArrowResultSet>,
    rows_affected: u64,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetcher")
            .field("budget", &self.budget)
            .field("overflow", &self.overflow)
            .field("result", &self.result)
            .field("rows_affected", &self.rows_affected)
            .field("done", &self.stream.is_none())
            .finish()
    }
}

impl Fetcher {
    /// Reads `stream` into a result of at most `budget` bytes.
    pub fn new(stream: ResultStream, budget: usize, overflow: Overflow) -> Self {
        Self {
            stream: Some(stream),
            budget,
            overflow,
            result: None,
            rows_affected: 0,
        }
    }

    /// Fetches until the run ends or the result reaches its budget.
    pub async fn fetch(&mut self) -> Result<FetchStatus, DbError> {
        let Some(stream) = &mut self.stream else {
            return Ok(FetchStatus::Done);
        };
        while let Some(frame) = stream.next().await {
            match frame? {
                Frame::Schema(schema) => self.result = Some(ArrowResultSet::new(schema)),
                Frame::Batch(batch) => {
                    let Some(result) = &mut self.result else {
                        return Err(DbError::General(
                            "the driver sent rows before their schema".to_string(),
                        ));
                    };
                    result.push(batch);
                    if result.memory_bytes() > self.budget {
                        return match self.overflow {
                            Overflow::Stop => Ok(FetchStatus::OverBudget),
                            Overflow::Spill => Err(DbError::General(
                                "the result is over its memory budget, and spilling it to disk \
                                 is not built yet"
                                    .to_string(),
                            )),
                        };
                    }
                }
                Frame::Affected(rows) => self.rows_affected += rows,
            }
        }
        self.stream = None;
        if let Some(result) = &mut self.result {
            result.finish();
        }
        Ok(FetchStatus::Done)
    }

    /// Raises the budget by `bytes` and fetches on: "fetch more".
    pub async fn fetch_more(&mut self, bytes: usize) -> Result<FetchStatus, DbError> {
        self.budget = self.budget.saturating_add(bytes);
        self.fetch().await
    }

    /// The last result set, so far.
    pub fn result(&self) -> Option<&ArrowResultSet> {
        self.result.as_ref()
    }

    /// The last result set, kept.
    pub fn into_result(self) -> Option<ArrowResultSet> {
        self.result
    }

    /// Rows changed by the run's statements so far.
    pub fn rows_affected(&self) -> u64 {
        self.rows_affected
    }

    /// True once the run has ended.
    pub fn is_done(&self) -> bool {
        self.stream.is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int64Array, RecordBatch};
    use arrow_schema::DataType;
    use futures_util::stream;

    use super::*;
    use crate::{
        arrow::{field, schema},
        result::ResultSet,
    };

    fn ints(n: i64) -> Vec<Result<Frame, DbError>> {
        let schema = schema(vec![field("n", DataType::Int64, "int8")]);
        let mut frames = vec![Ok(Frame::Schema(schema.clone()))];
        for start in (0..n).step_by(1000) {
            let array: ArrayRef =
                Arc::new(Int64Array::from_iter_values(start..n.min(start + 1000)));
            frames.push(Ok(Frame::Batch(
                RecordBatch::try_new(schema.clone(), vec![array]).expect("batch"),
            )));
        }
        frames
    }

    fn fetcher(frames: Vec<Result<Frame, DbError>>, budget: usize, overflow: Overflow) -> Fetcher {
        Fetcher::new(Box::pin(stream::iter(frames)), budget, overflow)
    }

    #[tokio::test]
    async fn a_run_is_read_to_the_end() {
        let mut frames = vec![Ok(Frame::Affected(2))];
        frames.extend(ints(2500));
        frames.push(Ok(Frame::Affected(3)));
        let mut f = fetcher(frames, DEFAULT_BUDGET, Overflow::Stop);
        assert_eq!(f.fetch().await.expect("fetch"), FetchStatus::Done);
        assert!(f.is_done());
        assert_eq!(f.rows_affected(), 5);
        let result = f.result().expect("result");
        assert_eq!(result.row_count(), 2500);
        assert!(result.is_complete());
        // Fetching again does nothing.
        assert_eq!(f.fetch().await.expect("fetch"), FetchStatus::Done);
        assert!(format!("{f:?}").contains("done: true"));
        assert_eq!(f.into_result().map(|r| r.row_count()), Some(2500));
    }

    #[tokio::test]
    async fn the_budget_stops_fetching_until_more_is_asked_for() {
        // Each batch of 1000 i64 is about 8 KB.
        let mut f = fetcher(ints(10_000), 20_000, Overflow::Stop);
        assert_eq!(f.fetch().await.expect("fetch"), FetchStatus::OverBudget);
        let rows = f.result().expect("result").row_count();
        assert!((2000..10_000).contains(&rows), "{rows}");
        assert!(!f.result().expect("result").is_complete());
        assert_eq!(
            f.fetch_more(usize::MAX).await.expect("fetch"),
            FetchStatus::Done
        );
        assert_eq!(f.result().expect("result").row_count(), 10_000);
    }

    #[tokio::test]
    async fn spilling_is_not_built_yet() {
        let mut f = fetcher(ints(10_000), 20_000, Overflow::Spill);
        let err = f.fetch().await.expect_err("an error");
        assert!(err.to_string().contains("not built yet"), "{err}");
    }

    #[tokio::test]
    async fn the_last_result_set_wins_and_errors_end_the_run() {
        let mut frames = ints(10);
        frames.extend(ints(3));
        frames.push(Err(DbError::General("boom".into())));
        let mut f = fetcher(frames, DEFAULT_BUDGET, Overflow::Stop);
        assert!(f.fetch().await.is_err());
        assert_eq!(f.result().expect("result").row_count(), 3);
    }

    #[tokio::test]
    async fn rows_need_a_schema_first() {
        let frames = ints(5).into_iter().skip(1).collect();
        let mut f = fetcher(frames, DEFAULT_BUDGET, Overflow::Stop);
        assert!(f.fetch().await.is_err());
    }
}
