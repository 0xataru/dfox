//! SQLite files of the user's projects wherever they live, whatever their
//! folders are called: the OS's file index (Spotlight, `locate`), the
//! folders dfox was started in before, `sqlite_folders` in the config, and
//! without an index, the usual project folders. Files from the index are
//! taken by their path and never opened, so macOS asks for no access to
//! Documents or Desktop; files from walks are told by their header.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use dfox_core::models::connections::DbType;

use super::Connection;
use crate::components::manager::Detected;

const TIMEOUT: Duration = Duration::from_secs(3);
/// Files listed at most.
const MAX: usize = 100;
/// Folders remembered from the starts, the latest last.
const REMEMBERED: usize = 20;
const FOLDERS_FILE: &str = "folders";
/// Where projects usually live, walked when there is no index.
const USUAL: &[&str] = &[
    "dev",
    "code",
    "projects",
    "src",
    "work",
    "Developer",
    "repos",
    "workspace",
];
/// Folders that hold dependencies, builds and caches, not projects' data.
const SKIP: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "venv",
    "build",
    "dist",
    "testdata",
    "__pycache__",
    "site-packages",
    "Library",
    "Applications",
    "Caches",
    "snap",
];

fn home() -> Option<PathBuf> {
    Some(directories::BaseDirs::new()?.home_dir().to_path_buf())
}

/// The SQLite files found now, searching `folders` besides the index.
pub async fn find(folders: Vec<PathBuf>) -> Vec<Detected> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let indexed = index(&home).await;
    let walk_home = home.clone();
    let walked = tokio::task::spawn_blocking(move || {
        let mut found = BTreeSet::new();
        let usual: Vec<PathBuf> = if indexed.is_none() {
            USUAL.iter().map(|dir| walk_home.join(dir)).collect()
        } else {
            Vec::new()
        };
        for dir in folders.iter().chain(&usual) {
            walk(dir, 6, &mut found);
        }
        (indexed, found)
    })
    .await;
    let Ok((indexed, walked)) = walked else {
        return Vec::new();
    };
    let indexed: BTreeSet<PathBuf> = indexed
        .unwrap_or_default()
        .into_iter()
        .filter(|path| !is_junk(path, &home))
        .collect();
    indexed
        .union(&walked)
        .take(MAX)
        .map(|path| detected(path, &home))
        .collect()
}

/// The OS's file index, for every file named like a SQLite database under
/// the home folder: None when there is no index to ask.
async fn index(home: &Path) -> Option<Vec<PathBuf>> {
    let names =
        "kMDItemFSName == '*.db' || kMDItemFSName == '*.sqlite' || kMDItemFSName == '*.sqlite3'";
    let home_arg = home.display().to_string();
    let attempts: [(&str, Vec<&str>); 3] = [
        ("mdfind", vec!["-onlyin", &home_arg, names]),
        ("plocate", vec!["-e", "--regex", r"\.(db|sqlite|sqlite3)$"]),
        ("locate", vec!["-e", "--regex", r"\.(db|sqlite|sqlite3)$"]),
    ];
    for (command, args) in attempts {
        let run = tokio::process::Command::new(command)
            .args(&args)
            .kill_on_drop(true)
            .output();
        if let Ok(Ok(out)) = tokio::time::timeout(TIMEOUT, run).await {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                return Some(text.lines().map(PathBuf::from).collect());
            }
        }
    }
    None
}

/// A path that belongs to the system, an app, a dependency or a cache
/// rather than to a project: outside the home folder, in a hidden folder,
/// in an app bundle or library, or in a folder of [`SKIP`].
pub fn is_junk(path: &Path, home: &Path) -> bool {
    let Ok(inside) = path.strip_prefix(home) else {
        return true;
    };
    if inside.starts_with("go/pkg") {
        return true;
    }
    let parts: Vec<String> = inside
        .parent()
        .map(|p| p.iter().map(|c| c.to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    parts.iter().any(|part| {
        part.starts_with('.')
            || SKIP.contains(&part.as_str())
            || part.ends_with(".app")
            || part.to_lowercase().ends_with("library")
    })
}

fn is_sqlite_name(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| matches!(e.to_str(), Some("db" | "sqlite" | "sqlite3")))
}

/// SQLite files under `dir`, `depth` folders down at most, told by their
/// header.
fn walk(dir: &Path, depth: usize, found: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let skip =
                name.starts_with('.') || SKIP.contains(&name.as_str()) || name.ends_with(".app");
            if depth > 0 && !skip {
                walk(&path, depth - 1, found);
            }
        } else if kind.is_file() && is_sqlite_name(&path) && is_sqlite(&path) {
            found.insert(path);
        }
    }
}

pub fn is_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok_and(|()| &header == b"SQLite format 3\0")
}

pub fn detected(path: &Path, home: &Path) -> Detected {
    let shown = match path.strip_prefix(home) {
        Ok(inside) => format!("~/{}", inside.display()),
        Err(_) => path.display().to_string(),
    };
    let mut connection = Connection::new(&shown, DbType::Sqlite);
    connection.database = Some(path.display().to_string());
    Detected {
        connection,
        password: None,
        source: "a SQLite file on this machine".into(),
    }
}

/// The folders dfox was started in before, the latest last.
pub fn remembered(state_dir: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(state_dir.join(FOLDERS_FILE))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Remembers the folder dfox starts in, so its SQLite files are found from
/// anywhere next time. The home folder and the root are too wide to walk.
pub fn remember(state_dir: &Path, folder: &Path) {
    if home().as_deref() == Some(folder) || folder.parent().is_none() {
        return;
    }
    let mut folders = remembered(state_dir);
    folders.retain(|f| f != folder);
    folders.push(folder.to_path_buf());
    let skip = folders.len().saturating_sub(REMEMBERED);
    let text: Vec<String> = folders
        .iter()
        .skip(skip)
        .map(|f| f.display().to_string())
        .collect();
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(state_dir.join(FOLDERS_FILE), text.join("\n") + "\n");
}

/// `~/x` as the home folder's `x`.
pub fn expand(folder: &str) -> Option<PathBuf> {
    match folder.strip_prefix("~/") {
        Some(rest) => Some(home()?.join(rest)),
        None => Some(PathBuf::from(folder)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_app_and_dependency_files_are_left_out() {
        let home = Path::new("/Users/me");
        let junk = |path: &str| is_junk(Path::new(path), home);
        assert!(!junk("/Users/me/dev/jobster/jobster.db"));
        assert!(!junk("/Users/me/12-е9р1зйпшлцжт/shop/data/app.sqlite"));
        assert!(junk("/System/Library/x.db"));
        assert!(junk("/Users/Shared/Battle.net/Agent/product.db"));
        assert!(junk("/Users/me/Library/Application Support/Slack/x.db"));
        assert!(junk("/Users/me/go/pkg/mod/modernc.org/sqlite/embed.db"));
        assert!(junk("/Users/me/.cache/x.sqlite"));
        assert!(junk("/Users/me/dev/app/node_modules/x/test.db"));
        assert!(junk(
            "/Users/me/Pictures/Photos Library.photoslibrary/database/Photos.sqlite"
        ));
        assert!(junk("/Users/me/dev/x/testdata/fuzz.db"));
    }

    #[test]
    fn walks_tell_files_by_header_and_skip_dependencies() {
        let dir = std::env::temp_dir().join(format!("dfox-sqlite-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut header = b"SQLite format 3\0".to_vec();
        header.extend([0u8; 84]);
        for (path, bytes) in [
            ("random-name/shop/state/app.db", &header[..]),
            ("random-name/shop/node_modules/x/test.db", &header[..]),
            ("random-name/shop/.cache/c.sqlite", &header[..]),
            ("random-name/notes.db", b"plain text, not sqlite".as_slice()),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, bytes).expect("write");
        }
        let mut found = BTreeSet::new();
        walk(&dir, 6, &mut found);
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            [dir.join("random-name/shop/state/app.db")]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn started_in_folders_are_remembered_latest_last() {
        let state = std::env::temp_dir().join(format!("dfox-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        remember(&state, Path::new("/p/a"));
        remember(&state, Path::new("/p/b"));
        remember(&state, Path::new("/p/a"));
        remember(&state, Path::new("/"));
        assert_eq!(
            remembered(&state),
            [PathBuf::from("/p/b"), PathBuf::from("/p/a")]
        );
        let _ = std::fs::remove_dir_all(&state);
    }
}
