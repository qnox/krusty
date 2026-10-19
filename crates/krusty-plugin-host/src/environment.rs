//! What provisioning reads from the machine it runs on. Tests build one directly; a driver reads it
//! from its own process with [`Environment::from_process`].

use std::ffi::OsString;
use std::path::PathBuf;

const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2";

/// An operating system and CPU, spelled as Rust's `std::env::consts` spells them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

impl Platform {
    pub fn current() -> Self {
        Platform {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }
}

#[derive(Clone, Debug)]
pub struct Environment {
    pub platform: Platform,
    /// `JAVA_HOME`.
    pub java_home: Option<PathBuf>,
    /// `PATH`, searched for `java`.
    pub path: Option<OsString>,
    /// The Gradle user home (`GRADLE_USER_HOME`, else `~/.gradle`), whose module cache may already
    /// hold the compiler jars.
    pub gradle_user_home: Option<PathBuf>,
    /// The local Maven repository (`~/.m2/repository`).
    pub maven_local: Option<PathBuf>,
    /// Where provisioning keeps what it downloads or copies.
    pub cache_dir: PathBuf,
    /// The Maven repository jars are downloaded from, without a trailing `/`.
    pub maven_repository: String,
    /// Fail instead of downloading.
    pub offline: bool,
}

impl Environment {
    /// Reads the process environment:
    /// - `KRUSTY_CACHE_DIR` overrides the cache, otherwise the platform's user cache directory;
    /// - `KRUSTY_MAVEN_REPOSITORY` replaces Maven Central, for a mirror; the pinned digests still
    ///   decide what is accepted;
    /// - `KRUSTY_OFFLINE` set to anything but empty, `0` or `false` turns downloads off.
    ///
    /// `None` when no cache directory can be named: no `KRUSTY_CACHE_DIR` and no home directory.
    pub fn from_process() -> Option<Self> {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let platform = Platform::current();
        let home = var(if platform.is_windows() {
            "USERPROFILE"
        } else {
            "HOME"
        })
        .map(PathBuf::from);
        let user_cache = if platform.is_windows() {
            var("LOCALAPPDATA").map(PathBuf::from)
        } else if platform.os == "macos" {
            home.as_ref().map(|home| home.join("Library/Caches"))
        } else {
            var("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(".cache")))
        };
        let cache_dir = match var("KRUSTY_CACHE_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => user_cache?.join("krusty"),
        }
        .join("plugin-host");
        let offline = var("KRUSTY_OFFLINE").is_some_and(|value| value != "0" && value != "false");
        Some(Environment {
            java_home: var("JAVA_HOME").map(PathBuf::from),
            path: var("PATH"),
            gradle_user_home: var("GRADLE_USER_HOME")
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(".gradle"))),
            maven_local: home.as_ref().map(|home| home.join(".m2/repository")),
            cache_dir,
            maven_repository: var("KRUSTY_MAVEN_REPOSITORY")
                .and_then(|value| value.into_string().ok())
                .map(|value| value.trim_end_matches('/').to_string())
                .unwrap_or_else(|| MAVEN_CENTRAL.to_string()),
            offline,
            platform,
        })
    }
}
