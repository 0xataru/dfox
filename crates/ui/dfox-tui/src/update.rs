//! Tells when a newer dfox is out. GitHub is asked at most once a day, in
//! the background; the answer is kept in `latest-version` in the state
//! directory and shown from the next start. `update_check = false` in
//! `config.toml` turns it off.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use tokio::sync::oneshot;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const HOST: &str = "api.github.com";
const REQUEST: &[u8] = b"GET /repos/0xataru/dfox/releases/latest HTTP/1.0\r\n\
Host: api.github.com\r\nUser-Agent: dfox\r\nAccept: application/vnd.github+json\r\n\r\n";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const TIMEOUT: Duration = Duration::from_secs(10);
const FILE: &str = "latest-version";

/// The newer version and how to get it, as the status bar says it, when the
/// last check found one: `0.3.6 is out: brew update && brew upgrade dfox`.
pub fn notice(state_dir: Option<&Path>) -> Option<String> {
    let latest = std::fs::read_to_string(state_dir?.join(FILE)).ok()?;
    let latest = latest.trim();
    if !newer(env!("CARGO_PKG_VERSION"), latest) {
        return None;
    }
    let how =
        std::env::current_exe().map_or(RELEASES, |exe| how_to_upgrade(&exe, Path::new("/etc")));
    Some(format!("{latest} is out: {how}"))
}

const RELEASES: &str = "github.com/0xataru/dfox/releases";

/// How to upgrade the dfox at `exe`, told by how it was installed. `etc`
/// holds the package managers' settings.
fn how_to_upgrade(exe: &Path, etc: &Path) -> &'static str {
    // Homebrew starts dfox through a link in its bin folder; the file itself
    // is in the Cellar.
    let exe = &exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    if exe.to_string_lossy().contains("/Cellar/") {
        return "brew update && brew upgrade dfox";
    }
    // From the APT or dnf repository, once it is set up as the README says.
    if exe == Path::new("/usr/bin/dfox") {
        if etc.join("apt/sources.list.d/dfox.list").exists() {
            return "sudo apt update && sudo apt upgrade dfox";
        }
        if etc.join("yum.repos.d/dfox.repo").exists() {
            return "sudo dnf upgrade dfox";
        }
    }
    // The install scripts put their updater next to dfox.
    let updater = if cfg!(windows) {
        "dfox-update.exe"
    } else {
        "dfox-update"
    };
    if exe.with_file_name(updater).exists() {
        return "dfox-update";
    }
    RELEASES
}

/// Asks GitHub for the latest release in the background, unless that was
/// done less than a day ago. The answer comes back as the notice to show, so
/// the dfox that asked shows it at once.
pub fn refresh(state_dir: Option<PathBuf>) -> Option<oneshot::Receiver<Option<String>>> {
    let path = state_dir.map(|dir| dir.join(FILE))?;
    let checked = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if checked
        .is_some_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < CHECK_EVERY)
    {
        return None;
    }
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let latest = match tokio::time::timeout(TIMEOUT, latest()).await {
            Ok(Ok(version)) => version,
            Ok(Err(err)) => {
                log::info!("update check: {err}");
                // Keep the last answer, and wait a day before asking again.
                std::fs::read_to_string(&path).unwrap_or_default()
            }
            Err(_) => {
                log::info!("update check: timed out");
                std::fs::read_to_string(&path).unwrap_or_default()
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, latest.trim());
        let _ = tx.send(notice(path.parent()));
    });
    Some(rx)
}

/// The version of the latest GitHub release, without the `v`.
async fn latest() -> Result<String, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|err| err.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
    let server = ServerName::try_from(HOST).map_err(|err| err.to_string())?;
    let tcp = TcpStream::connect((HOST, 443))
        .await
        .map_err(|err| err.to_string())?;
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(server, tcp)
        .await
        .map_err(|err| err.to_string())?;
    tls.write_all(REQUEST)
        .await
        .map_err(|err| err.to_string())?;
    let mut response = Vec::new();
    // HTTP/1.0: the server closes the connection after the body. Some close
    // without TLS's goodbye, which is fine once the response is in.
    if let Err(err) = tls.read_to_end(&mut response).await {
        if err.kind() != std::io::ErrorKind::UnexpectedEof || response.is_empty() {
            return Err(err.to_string());
        }
    }
    version_in(&String::from_utf8_lossy(&response))
}

/// The release's version from GitHub's HTTP response.
fn version_in(response: &str) -> Result<String, String> {
    let (head, body) = response.split_once("\r\n\r\n").ok_or("no HTTP response")?;
    let status = head.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") {
        return Err(format!("GitHub answered {status}"));
    }
    let release: serde_json::Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    let tag = release["tag_name"].as_str().ok_or("no tag_name")?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// Whether `latest` is a newer `major.minor.patch` than `current`.
fn newer(current: &str, latest: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        let parts: Vec<u64> = v
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?;
        (parts.len() == 3).then_some(parts)
    };
    matches!((parse(current), parse(latest)), (Some(c), Some(l)) if l > c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_upgrade_command_matches_how_dfox_was_installed() {
        let dir = std::env::temp_dir().join(format!("dfox-upgrade-{}", std::process::id()));
        let etc = dir.join("etc");
        std::fs::create_dir_all(&etc).expect("dir");
        let how = |exe: &Path| how_to_upgrade(exe, &etc);

        let brew = Path::new("/opt/homebrew/Cellar/dfox/0.3.5/bin/dfox");
        assert_eq!(how(brew), "brew update && brew upgrade dfox");
        // Started through Homebrew's link, as it always is.
        #[cfg(unix)]
        {
            let cellar = dir.join("Cellar/dfox/0.4.1/bin");
            std::fs::create_dir_all(&cellar).expect("cellar");
            std::fs::write(cellar.join("dfox"), "").expect("exe");
            std::fs::create_dir_all(dir.join("bin")).expect("bin");
            std::os::unix::fs::symlink(cellar.join("dfox"), dir.join("bin/dfox")).expect("link");
            assert_eq!(
                how(&dir.join("bin/dfox")),
                "brew update && brew upgrade dfox"
            );
        }

        let exe = dir.join("dfox");
        assert_eq!(how(&exe), RELEASES);
        let updater = if cfg!(windows) {
            "dfox-update.exe"
        } else {
            "dfox-update"
        };
        std::fs::write(dir.join(updater), "").expect("updater");
        assert_eq!(how(&exe), "dfox-update");

        // A .deb or .rpm installed by hand: no repository to upgrade from.
        let system = Path::new("/usr/bin/dfox");
        assert_eq!(how(system), RELEASES);
        std::fs::create_dir_all(etc.join("yum.repos.d")).expect("dir");
        std::fs::write(etc.join("yum.repos.d/dfox.repo"), "").expect("repo");
        assert_eq!(how(system), "sudo dnf upgrade dfox");
        std::fs::create_dir_all(etc.join("apt/sources.list.d")).expect("dir");
        std::fs::write(etc.join("apt/sources.list.d/dfox.list"), "").expect("list");
        assert_eq!(how(system), "sudo apt update && sudo apt upgrade dfox");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_higher_release_version_is_newer() {
        assert!(newer("0.3.5", "0.3.6"));
        assert!(newer("0.3.5", "0.4.0"));
        assert!(newer("0.9.9", "0.10.0"));
        assert!(!newer("0.3.5", "0.3.5"));
        assert!(!newer("0.3.6", "0.3.5"));
        assert!(!newer("0.3.5", ""));
        assert!(!newer("0.3.5", "0.4.0-rc.1"));
    }

    #[test]
    fn the_version_comes_from_the_release_tag() {
        let ok = "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"tag_name\":\"v0.3.6\",\"name\":\"v0.3.6\"}";
        assert_eq!(version_in(ok), Ok("0.3.6".into()));
        let limited = "HTTP/1.0 403 Forbidden\r\n\r\n{}";
        assert_eq!(
            version_in(limited),
            Err("GitHub answered HTTP/1.0 403 Forbidden".into())
        );
        assert!(version_in("garbage").is_err());
    }
}
