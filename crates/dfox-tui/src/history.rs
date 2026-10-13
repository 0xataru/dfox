//! Every query run, appended to `history.jsonl` in the dfox state directory:
//! `$DFOX_STATE_DIR`, else `$XDG_STATE_HOME/dfox` or `~/.local/state/dfox`
//! (`%LOCALAPPDATA%\dfox` on Windows). One JSON object per line.

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use serde::{Deserialize, Serialize};

/// Entries read at startup; older ones stay in the file.
const KEEP: usize = 1000;
/// The file is rewritten with only the last [`KEEP`] entries once it grows
/// past this many.
const COMPACT_AT: usize = 5 * KEEP;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub connection: String,
    pub database: String,
    pub sql: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    pub ms: u64,
}

impl Entry {
    pub fn now(connection: &str, database: &str, sql: &str) -> Self {
        Self {
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            connection: connection.to_string(),
            database: database.to_string(),
            sql: sql.to_string(),
            ok: true,
            rows: None,
            ms: 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct History {
    /// `None` keeps the history in memory only.
    path: Option<PathBuf>,
    entries: Vec<Entry>,
}

impl History {
    /// The dfox state directory, if one can be found.
    pub fn dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("DFOX_STATE_DIR") {
            return Some(PathBuf::from(dir));
        }
        if cfg!(windows) {
            return directories::BaseDirs::new().map(|dirs| dirs.data_local_dir().join("dfox"));
        }
        let base = match std::env::var_os("XDG_STATE_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => directories::BaseDirs::new()?
                .home_dir()
                .join(".local")
                .join("state"),
        };
        Some(base.join("dfox"))
    }

    /// Reads the history in `dir`. Lines that can't be read are skipped and
    /// reported once.
    pub fn load(dir: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = dir.map(|dir| dir.join("history.jsonl")) else {
            return (Self::default(), None);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                let problem = format!("can't read {}: {err}", path.display());
                return (Self::default(), Some(problem));
            }
        };
        let mut skipped = 0;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let entries: Vec<Entry> = lines[lines.len().saturating_sub(KEEP)..]
            .iter()
            .filter_map(|line| {
                let entry = serde_json::from_str(line).ok();
                skipped += usize::from(entry.is_none());
                entry
            })
            .collect();
        let history = Self {
            path: Some(path),
            entries,
        };
        let mut problem = (skipped > 0).then(|| {
            format!(
                "{}: skipped {skipped} unreadable line(s)",
                history.path_label()
            )
        });
        if lines.len() > COMPACT_AT {
            if let Err(err) = history.rewrite() {
                problem = Some(err);
            }
        }
        (history, problem)
    }

    fn path_label(&self) -> String {
        self.path
            .as_ref()
            .map_or_else(|| "history".to_string(), |p| p.display().to_string())
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Records `entry` and appends it to the file.
    pub fn push(&mut self, entry: Entry) -> Result<(), String> {
        let line = serde_json::to_string(&entry).map_err(|err| err.to_string())?;
        self.entries.push(entry);
        if self.entries.len() > KEEP {
            self.entries.remove(0);
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let append = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = open_private(path, false)?;
            writeln!(file, "{line}")
        };
        append().map_err(|err| format!("can't write {}: {err}", path.display()))
    }

    fn rewrite(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let write = || -> std::io::Result<()> {
            let tmp = path.with_extension("jsonl.tmp");
            let mut file = open_private(&tmp, true)?;
            for entry in &self.entries {
                let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
                writeln!(file, "{line}")?;
            }
            drop(file);
            std::fs::rename(&tmp, path)
        };
        write().map_err(|err| format!("can't write {}: {err}", path.display()))
    }

    /// Distinct queries matching `query`, best match first, then newest.
    /// An empty query lists them newest first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Entry> {
        let mut seen = std::collections::HashSet::new();
        let newest: Vec<&Entry> = self
            .entries
            .iter()
            .rev()
            .filter(|e| seen.insert(e.sql.trim()))
            .collect();
        if query.trim().is_empty() {
            return newest.into_iter().take(limit).collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = newest
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                pattern
                    .score(Utf32Str::new(&e.sql, &mut buf), &mut matcher)
                    .map(|score| (score, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, i)| newest[i])
            .collect()
    }
}

/// Opens a file only its owner can read: queries can hold secrets.
fn open_private(path: &Path, truncate: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    if truncate {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dfox-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entry(sql: &str) -> Entry {
        Entry::now("local", "shop", sql)
    }

    #[test]
    fn entries_survive_a_restart() {
        let dir = temp_dir("restart");
        let (mut history, problem) = History::load(Some(&dir));
        assert_eq!(problem, None);
        history.push(entry("select 1")).expect("push");
        history
            .push(Entry {
                ok: false,
                rows: Some(3),
                ..entry("select 2")
            })
            .expect("push");

        let (history, problem) = History::load(Some(&dir));
        assert_eq!(problem, None);
        let sql: Vec<&str> = history.entries().iter().map(|e| e.sql.as_str()).collect();
        assert_eq!(sql, ["select 1", "select 2"]);
        assert!(!history.entries()[1].ok);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("history.jsonl"))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_lines_are_skipped_and_reported() {
        let dir = temp_dir("broken");
        std::fs::create_dir_all(&dir).expect("dir");
        let good = serde_json::to_string(&entry("select 1")).expect("json");
        std::fs::write(dir.join("history.jsonl"), format!("{good}\nnot json\n\n")).expect("write");

        let (history, problem) = History::load(Some(&dir));
        assert_eq!(history.entries().len(), 1);
        assert!(problem.expect("a problem").contains("skipped 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_file_is_compacted() {
        let dir = temp_dir("compact");
        std::fs::create_dir_all(&dir).expect("dir");
        let line = serde_json::to_string(&entry("select 1")).expect("json");
        let text = format!("{line}\n").repeat(COMPACT_AT + 1);
        std::fs::write(dir.join("history.jsonl"), text).expect("write");

        let (history, problem) = History::load(Some(&dir));
        assert_eq!(problem, None);
        assert_eq!(history.entries().len(), KEEP);
        let lines = std::fs::read_to_string(dir.join("history.jsonl"))
            .expect("read")
            .lines()
            .count();
        assert_eq!(lines, KEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_finds_distinct_queries_newest_first() {
        let mut history = History::default();
        for sql in [
            "select * from users",
            "select 1",
            "select * from orders",
            "select 1",
        ] {
            history.push(entry(sql)).expect("push");
        }
        let found = |query| -> Vec<String> {
            history
                .search(query, 10)
                .iter()
                .map(|e| e.sql.clone())
                .collect()
        };
        assert_eq!(
            found(""),
            ["select 1", "select * from orders", "select * from users"]
        );
        assert_eq!(found("users"), ["select * from users"]);
        assert_eq!(
            found("from"),
            ["select * from orders", "select * from users"]
        );
        assert_eq!(history.search("", 1).len(), 1);
    }

    #[test]
    fn only_the_latest_entries_are_kept_in_memory() {
        let mut history = History::default();
        for i in 0..KEEP + 5 {
            history.push(entry(&format!("select {i}"))).expect("push");
        }
        assert_eq!(history.entries().len(), KEEP);
        assert_eq!(history.entries()[0].sql, "select 5");
    }
}
