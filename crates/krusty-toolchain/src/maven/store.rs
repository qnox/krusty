//! Where resolution reads POMs, Gradle module metadata and artifacts: the toolchain's shared
//! artifact cache (`<cache root>/.m2.cache`) and, for projects that list `mavenLocal`, the local
//! Maven repository before it. Both are laid out as Maven repositories.

use std::path::{Path, PathBuf};

use super::Coordinates;

pub struct Store {
    /// The repositories read, in order; the cache is the last.
    repositories: Vec<PathBuf>,
}

impl Store {
    /// The store under the cache root `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            repositories: vec![root.join(".m2.cache")],
        }
    }

    /// The store under the cache root `root`, reading the local Maven repository `local` first.
    pub fn with_local(local: &Path, root: &Path) -> Self {
        Self {
            repositories: vec![local.to_path_buf(), root.join(".m2.cache")],
        }
    }

    /// The local Maven repository (`LocalM2RepositoryFinder`): the `<localRepository>` of
    /// `~/.m2/settings.xml`, else of `$M2_HOME/conf/settings.xml`, else `~/.m2/repository`.
    pub fn local_repository() -> Option<PathBuf> {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)?;
        let m2_home = std::env::var_os("M2_HOME")
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(path).join("conf").join("settings.xml"));
        let configured = [Some(home.join(".m2").join("settings.xml")), m2_home]
            .into_iter()
            .flatten()
            .find_map(|settings| local_repository_setting(&settings));
        Some(configured.unwrap_or_else(|| home.join(".m2").join("repository")))
    }

    /// The cache root the toolchain uses on this host (`AmperUserCacheRoot`): the
    /// `KOTLIN_SHARED_CACHE_DIR` (or deprecated `AMPER_SHARED_CACHE_DIR`) variable, else the
    /// platform's user cache directory.
    pub fn default_root() -> Option<PathBuf> {
        let variable = |name: &str| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        if let Some(root) =
            variable("KOTLIN_SHARED_CACHE_DIR").or_else(|| variable("AMPER_SHARED_CACHE_DIR"))
        {
            return Some(root);
        }
        let caches = if cfg!(windows) {
            variable("LOCALAPPDATA")?
        } else if cfg!(target_os = "macos") {
            variable("HOME")?.join("Library").join("Caches")
        } else {
            match variable("XDG_CACHE_HOME") {
                Some(caches) => caches,
                None => variable("HOME")?.join(".cache"),
            }
        };
        Some(caches.join("JetBrains").join("Kotlin"))
    }

    /// Where the cache keeps the file of `coordinates` with `extension`.
    pub fn path(&self, coordinates: &Coordinates, extension: &str) -> PathBuf {
        let cache = self.repositories.last().map(PathBuf::as_path);
        file_path(cache.unwrap_or(Path::new("")), coordinates, extension)
    }

    /// The text of the file of `coordinates` with `extension`, from the first repository that
    /// holds it.
    pub fn read(&self, coordinates: &Coordinates, extension: &str) -> Option<String> {
        self.repositories.iter().find_map(|repository| {
            std::fs::read_to_string(file_path(repository, coordinates, extension)).ok()
        })
    }
}

/// Where the Maven repository `repository` keeps the file of `coordinates` with `extension`.
fn file_path(repository: &Path, coordinates: &Coordinates, extension: &str) -> PathBuf {
    let mut path = repository.to_path_buf();
    path.extend(coordinates.group.split('.'));
    path.push(&coordinates.artifact);
    path.push(coordinates.version.as_deref().unwrap_or("unspecified"));
    path.push(format!("{}.{extension}", coordinates.file_stem()));
    path
}

/// The `<localRepository>` a Maven `settings.xml` sets, if it exists and sets one.
fn local_repository_setting(settings: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(settings).ok()?;
    let document = roxmltree::Document::parse(&text).ok()?;
    let value = document
        .root_element()
        .children()
        .find(|node| node.tag_name().name() == "localRepository")?
        .text()?
        .trim();
    (!value.is_empty()).then(|| PathBuf::from(value))
}
