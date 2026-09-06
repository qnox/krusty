//! Zed extension for `krusty-lsp`.
//!
//! Resolution precedence for the language server binary:
//!   1. `lsp.krusty-lsp.binary.path` in settings — used verbatim.
//!   2. `krusty-lsp` on `PATH` — used verbatim, never overridden by a download.
//!   3. Managed download of the prebuilt binary from the project's GitHub releases,
//!      re-checked on every server start so the user runs the freshest build.

use std::fs;

use zed_extension_api::{
    self as zed, settings::LspSettings, Architecture, DownloadedFileType, GithubReleaseOptions,
    LanguageServerId, LanguageServerInstallationStatus as Status, Os, Result,
};

const SERVER_BINARY: &str = "krusty-lsp";
const RELEASE_REPO: &str = "qnox/krusty";

/// Sentinel written into a version directory once the download *and* the
/// executable bit have both landed. A download that dies mid-transfer leaves a
/// truncated binary behind; without this stamp its mere presence would satisfy
/// the reuse check forever, and the server could never start again.
const INSTALL_STAMP: &str = ".krusty-install-ok";

struct KrustyExtension {
    cached_binary_path: Option<String>,
}

// ---- pure helpers (unit-tested natively) ----

/// Maps the running platform to the release target triple, or an error for an
/// unsupported platform (e.g. 32-bit x86).
fn target_triple(os: Os, arch: Architecture) -> Result<&'static str> {
    Ok(match (os, arch) {
        (Os::Mac, Architecture::Aarch64) => "aarch64-apple-darwin",
        (Os::Mac, Architecture::X8664) => "x86_64-apple-darwin",
        (Os::Linux, Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
        (Os::Linux, Architecture::X8664) => "x86_64-unknown-linux-gnu",
        (Os::Windows, Architecture::X8664) => "x86_64-pc-windows-msvc",
        (Os::Windows, Architecture::Aarch64) => "aarch64-pc-windows-msvc",
        _ => {
            return Err(
                "krusty-lsp has no prebuilt binary for the current platform; install krusty-lsp \
                 on PATH or set lsp.krusty-lsp.binary.path in settings.json"
                    .to_string(),
            )
        }
    })
}

/// Picks the `krusty-lsp` release asset for the given target triple. Matches by
/// name so the `v`-prefix / build-number version format cannot desync the lookup,
/// and so the compiler (`krusty-…`) and extension (`krusty-zed-…`) assets in the
/// same release are never mis-selected.
fn pick_asset_index(names: &[String], triple: &str, windows: bool) -> Option<usize> {
    let extension = if windows { ".zip" } else { ".tar.gz" };
    let infix = format!("-{triple}");
    names.iter().position(|name| {
        name.starts_with("krusty-lsp-") && name.ends_with(extension) && name.contains(&infix)
    })
}

/// The binary file name inside the extracted archive.
fn binary_filename(windows: bool) -> &'static str {
    if windows {
        "krusty-lsp.exe"
    } else {
        "krusty-lsp"
    }
}

/// Path of the completion sentinel inside a version directory.
fn install_stamp_path(version_dir: &str) -> String {
    format!("{version_dir}/{INSTALL_STAMP}")
}

/// Whether a cached version directory holds a *finished* install: the binary is
/// there and the stamp proves the download and `make_file_executable` both ran.
fn install_is_complete(version_dir: &str, windows: bool) -> bool {
    is_file(&format!("{version_dir}/{}", binary_filename(windows)))
        && is_file(&install_stamp_path(version_dir))
}

/// Records that a version directory is fully installed. Written last, so it can
/// only exist when every earlier step succeeded.
fn mark_install_complete(version_dir: &str, version: &str) -> Result<()> {
    fs::write(install_stamp_path(version_dir), version.as_bytes())
        .map_err(|err| format!("failed to record the krusty-lsp install stamp: {err}"))
}

/// The version directory a cached binary path sits in.
fn version_dir_of(binary_path: &str) -> Option<&str> {
    binary_path.rsplit_once('/').map(|(dir, _)| dir)
}

/// Highest-versioned *complete* install under `root`, as a `dir/binary` path
/// relative to it. Partial downloads are ignored rather than handed to Zed.
fn newest_complete_install(root: &std::path::Path, windows: bool) -> Option<String> {
    let mut dirs: Vec<String> = fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (name.starts_with("krusty-lsp-") && entry.path().is_dir()).then_some(name)
        })
        .collect();
    dirs.sort();
    dirs.into_iter().rev().find_map(|dir| {
        let full = root.join(&dir);
        install_is_complete(full.to_str()?, windows)
            .then(|| format!("{dir}/{}", binary_filename(windows)))
    })
}

// ---- managed download ----

impl KrustyExtension {
    fn ensure_managed_binary(&mut self, id: &LanguageServerId) -> Result<String> {
        zed::set_language_server_installation_status(id, &Status::CheckingForUpdate);

        let (os, arch) = zed::current_platform();
        let triple = target_triple(os, arch)?;
        let windows = triple.contains("windows");

        let release = match zed::latest_github_release(
            RELEASE_REPO,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        ) {
            Ok(release) => release,
            Err(err) => {
                // Offline / rate-limited: fall back to any binary already on disk.
                if let Some(path) = self.offline_binary(windows) {
                    zed::set_language_server_installation_status(id, &Status::None);
                    return Ok(path);
                }
                zed::set_language_server_installation_status(id, &Status::Failed(err.clone()));
                return Err(format!(
                    "could not fetch the latest krusty-lsp release and no downloaded copy is \
                     available: {err}. Install krusty-lsp on PATH or set \
                     lsp.krusty-lsp.binary.path in settings.json."
                ));
            }
        };

        let names: Vec<String> = release
            .assets
            .iter()
            .map(|asset| asset.name.clone())
            .collect();
        let index = pick_asset_index(&names, triple, windows).ok_or_else(|| {
            format!(
                "release {} has no krusty-lsp asset for {triple}",
                release.version
            )
        })?;
        let asset = &release.assets[index];

        let version_dir = format!("krusty-lsp-{}", release.version);
        let binary_path = format!("{version_dir}/{}", binary_filename(windows));

        if !install_is_complete(&version_dir, windows) {
            zed::set_language_server_installation_status(id, &Status::Downloading);
            // An earlier attempt may have left a partial extraction here; drop it
            // rather than downloading over the top of it.
            let _ = fs::remove_dir_all(&version_dir);
            let file_type = if windows {
                DownloadedFileType::Zip
            } else {
                DownloadedFileType::GzipTar
            };
            zed::download_file(&asset.download_url, &version_dir, file_type).map_err(|err| {
                zed::set_language_server_installation_status(id, &Status::Failed(err.clone()));
                format!("failed to download krusty-lsp: {err}")
            })?;
            zed::make_file_executable(&binary_path)
                .map_err(|err| format!("failed to mark krusty-lsp executable: {err}"))?;
            mark_install_complete(&version_dir, &release.version)?;
            remove_stale_versions(&version_dir);
        }

        zed::set_language_server_installation_status(id, &Status::None);
        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    /// Newest downloaded binary to use when GitHub is unreachable: this session's
    /// cached path if its install is still complete, otherwise the highest-sorting
    /// complete version. A half-downloaded directory is never offered.
    fn offline_binary(&self, windows: bool) -> Option<String> {
        if let Some(path) = &self.cached_binary_path {
            if version_dir_of(path).is_some_and(|dir| install_is_complete(dir, windows)) {
                return Some(path.clone());
            }
        }
        newest_complete_install(std::path::Path::new("."), windows)
    }
}

fn is_file(path: &str) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Drops every downloaded version directory except the one just installed.
fn remove_stale_versions(keep: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with("krusty-lsp-") && name != keep && entry.path().is_dir() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

// ---- extension trait ----

impl zed::Extension for KrustyExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let command = match binary
            .as_ref()
            .and_then(|binary| binary.path.clone())
            .or_else(|| worktree.which(SERVER_BINARY))
        {
            Some(path) => path,
            None => self.ensure_managed_binary(language_server_id)?,
        };

        let mut args = binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();
        if !args.iter().any(|argument| argument == "--stdio") {
            args.insert(0, "--stdio".to_string());
        }

        let mut env = worktree.shell_env();
        if let Some(overrides) = binary.and_then(|binary| binary.env) {
            for (key, value) in overrides {
                env.retain(|(existing, _)| existing != &key);
                env.push((key, value));
            }
        }

        Ok(zed::Command { command, args, env })
    }
}

zed::register_extension!(KrustyExtension);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_triple_covers_every_shipped_target() {
        assert_eq!(
            target_triple(Os::Mac, Architecture::Aarch64).unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_triple(Os::Mac, Architecture::X8664).unwrap(),
            "x86_64-apple-darwin"
        );
        assert_eq!(
            target_triple(Os::Linux, Architecture::Aarch64).unwrap(),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Os::Linux, Architecture::X8664).unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Os::Windows, Architecture::X8664).unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            target_triple(Os::Windows, Architecture::Aarch64).unwrap(),
            "aarch64-pc-windows-msvc"
        );
    }

    #[test]
    fn target_triple_rejects_unsupported_platform() {
        assert!(target_triple(Os::Linux, Architecture::X86).is_err());
        assert!(target_triple(Os::Mac, Architecture::X86).is_err());
    }

    fn sample_assets() -> Vec<String> {
        vec![
            "krusty-2.4.20-build.3-aarch64-apple-darwin.tar.gz".to_string(),
            "krusty-lsp-2.4.20-build.3-aarch64-apple-darwin.tar.gz".to_string(),
            "krusty-lsp-2.4.20-build.3-x86_64-apple-darwin.tar.gz".to_string(),
            "krusty-lsp-2.4.20-build.3-x86_64-pc-windows-msvc.zip".to_string(),
            "krusty-lsp-2.4.20-build.3-aarch64-pc-windows-msvc.zip".to_string(),
            "krusty-zed-2.4.20-build.3.tar.gz".to_string(),
        ]
    }

    #[test]
    fn pick_asset_selects_lsp_for_triple() {
        let names = sample_assets();
        let index = pick_asset_index(&names, "aarch64-apple-darwin", false).unwrap();
        assert_eq!(
            names[index],
            "krusty-lsp-2.4.20-build.3-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn pick_asset_never_selects_compiler_or_extension_asset() {
        // Only the compiler + extension assets carry this triple-less shape; a
        // triple that has no lsp asset must yield None, not the krusty-… archive.
        let names = vec![
            "krusty-2.4.20-build.3-aarch64-apple-darwin.tar.gz".to_string(),
            "krusty-zed-2.4.20-build.3.tar.gz".to_string(),
        ];
        assert!(pick_asset_index(&names, "aarch64-apple-darwin", false).is_none());
    }

    #[test]
    fn pick_asset_honors_windows_zip_extension() {
        let names = sample_assets();
        let index = pick_asset_index(&names, "x86_64-pc-windows-msvc", true).unwrap();
        assert_eq!(
            names[index],
            "krusty-lsp-2.4.20-build.3-x86_64-pc-windows-msvc.zip"
        );
        // The same triple must not match when asking for a unix (.tar.gz) asset.
        assert!(pick_asset_index(&names, "x86_64-pc-windows-msvc", false).is_none());
    }

    #[test]
    fn pick_asset_returns_none_for_missing_triple() {
        let names = sample_assets();
        assert!(pick_asset_index(&names, "aarch64-unknown-linux-gnu", false).is_none());
    }

    #[test]
    fn binary_filename_matches_platform() {
        assert_eq!(binary_filename(true), "krusty-lsp.exe");
        assert_eq!(binary_filename(false), "krusty-lsp");
    }

    // ---- install stamp ----

    /// Unique scratch directory; the crate has no dev-dependencies by design.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("krusty-zed-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Lays out a version directory: `binary` writes the server file, `stamp`
    /// writes the completion sentinel.
    fn install(root: &std::path::Path, version: &str, binary: bool, stamp: bool) -> String {
        let dir = root.join(format!("krusty-lsp-{version}"));
        fs::create_dir_all(&dir).unwrap();
        if binary {
            fs::write(dir.join("krusty-lsp"), b"ELF").unwrap();
        }
        if stamp {
            fs::write(dir.join(INSTALL_STAMP), version.as_bytes()).unwrap();
        }
        dir.to_str().unwrap().to_string()
    }

    #[test]
    fn install_is_incomplete_when_the_directory_is_missing() {
        let root = scratch("missing");
        let dir = root.join("krusty-lsp-v1").to_str().unwrap().to_string();
        assert!(!install_is_complete(&dir, false));
    }

    /// The real incident: `download_file` died mid-transfer, leaving a truncated
    /// binary that never got chmod'd. Presence alone must not count as installed.
    #[test]
    fn install_is_incomplete_when_only_the_binary_is_present() {
        let root = scratch("partial");
        let dir = install(&root, "v1", true, false);
        assert!(!install_is_complete(&dir, false));
    }

    #[test]
    fn install_is_incomplete_when_only_the_stamp_is_present() {
        let root = scratch("stamp-only");
        let dir = install(&root, "v1", false, true);
        assert!(!install_is_complete(&dir, false));
    }

    #[test]
    fn install_is_complete_with_binary_and_stamp() {
        let root = scratch("complete");
        let dir = install(&root, "v1", true, true);
        assert!(install_is_complete(&dir, false));
    }

    #[test]
    fn install_is_complete_checks_the_windows_binary_name() {
        let root = scratch("windows");
        let dir = install(&root, "v1", true, true);
        // The unix binary and the stamp exist, but krusty-lsp.exe does not.
        assert!(!install_is_complete(&dir, true));
        fs::write(std::path::Path::new(&dir).join("krusty-lsp.exe"), b"MZ").unwrap();
        assert!(install_is_complete(&dir, true));
    }

    #[test]
    fn marking_an_install_records_the_version_and_completes_it() {
        let root = scratch("mark");
        let dir = install(&root, "v2.4.10-build.517", true, false);
        assert!(!install_is_complete(&dir, false));

        mark_install_complete(&dir, "v2.4.10-build.517").unwrap();

        assert!(install_is_complete(&dir, false));
        let stamp = fs::read_to_string(install_stamp_path(&dir)).unwrap();
        assert_eq!(stamp, "v2.4.10-build.517");
    }

    #[test]
    fn marking_an_install_reports_a_missing_directory() {
        let root = scratch("mark-missing");
        let dir = root.join("krusty-lsp-absent").to_str().unwrap().to_string();
        assert!(mark_install_complete(&dir, "v1").is_err());
    }

    // ---- offline fallback ----

    #[test]
    fn newest_complete_install_skips_a_partial_download() {
        // The incident shape: build.514 installed cleanly, build.516 truncated.
        let root = scratch("offline-partial");
        install(&root, "v2.4.10-build.514", true, true);
        install(&root, "v2.4.10-build.516", true, false);

        assert_eq!(
            newest_complete_install(&root, false),
            Some("krusty-lsp-v2.4.10-build.514/krusty-lsp".to_string())
        );
    }

    #[test]
    fn newest_complete_install_prefers_the_highest_version() {
        let root = scratch("offline-newest");
        install(&root, "v2.4.10-build.514", true, true);
        install(&root, "v2.4.10-build.516", true, true);

        assert_eq!(
            newest_complete_install(&root, false),
            Some("krusty-lsp-v2.4.10-build.516/krusty-lsp".to_string())
        );
    }

    #[test]
    fn newest_complete_install_is_none_when_nothing_is_installed() {
        let root = scratch("offline-empty");
        install(&root, "v2.4.10-build.516", true, false);
        assert_eq!(newest_complete_install(&root, false), None);
    }

    #[test]
    fn version_dir_of_splits_a_cached_binary_path() {
        assert_eq!(
            version_dir_of("krusty-lsp-v1/krusty-lsp"),
            Some("krusty-lsp-v1")
        );
        assert_eq!(version_dir_of("krusty-lsp"), None);
    }
}
