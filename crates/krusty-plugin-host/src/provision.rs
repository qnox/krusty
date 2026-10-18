//! Finding or fetching the JDK and compiler jars one host runs on, telling the user what is fetched
//! and why.

use std::io::Write;
use std::path::PathBuf;

use crate::artifacts;
use crate::download::Fetcher;
use crate::environment::Environment;
use crate::install::{self, Mismatch};
use crate::jdk::{self, Jdk};
use crate::pins::{ArtifactPin, JdkPin, Pins};

/// The oldest Java that runs kotlinc 2.4's own classes, which are class-file version 52.
pub const COMPILER_MIN_JAVA: u32 = 8;

pub struct Request<'a> {
    /// The build's Kotlin version, exactly as configured.
    pub kotlin_version: &'a str,
    /// What needs the host, as the user should read it, such as
    /// `the compiler plugin compose-compiler-plugin-embeddable-2.4.20.jar`.
    pub reason: &'a str,
    /// The oldest Java the plugins can run on. Below [`COMPILER_MIN_JAVA`] it is raised to it.
    pub min_java: u32,
}

/// What a host process is started with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRuntime {
    /// The Kotlin version the compiler classpath is, exactly as the build configured it.
    pub kotlin_version: String,
    pub java: PathBuf,
    pub java_home: PathBuf,
    /// `kotlin-compiler-embeddable` first, then its runtime dependencies.
    pub compiler_classpath: Vec<PathBuf>,
    /// [`ArtifactPin::identity`] of the pins the classpath was verified against, which names what is
    /// built against it, such as the host's adapter.
    pub compiler_identity: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProvisionError {
    /// No compiler classpath is pinned for this exact Kotlin version.
    UnpinnedKotlin {
        version: String,
        pinned: Vec<String>,
    },
    /// No installed JDK is new enough, and none can be downloaded for this platform.
    NoJdk {
        reason: String,
        needed: u32,
        rejected: Vec<String>,
        platform: String,
    },
    /// Something is neither installed nor cached, and downloads are off.
    Offline {
        reason: String,
        missing: Vec<String>,
        java: Option<u32>,
    },
    Download {
        url: String,
        detail: String,
    },
    /// A download arrived, but it is not the pinned file.
    Corrupt {
        url: String,
        detail: String,
    },
    Io {
        path: PathBuf,
        detail: String,
    },
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProvisionError::UnpinnedKotlin { version, pinned } => write!(
                f,
                "krusty cannot host compiler plugins for Kotlin {version}: it has no pinned \
                 kotlin-compiler-embeddable classpath for that exact version (pinned: {})",
                pinned.join(", ")
            ),
            ProvisionError::NoJdk {
                reason,
                needed,
                rejected,
                platform,
            } => write!(
                f,
                "{reason} runs in a JVM, but no installed JDK is Java {needed} or newer ({}), and \
                 krusty has no JDK to download for {platform}. Set JAVA_HOME to a Java {needed} or \
                 newer JDK",
                rejections(rejected)
            ),
            ProvisionError::Offline {
                reason,
                missing,
                java,
            } => {
                write!(
                    f,
                    "{reason} needs files that are neither installed nor cached, and downloads are \
                     off (KRUSTY_OFFLINE): {}. Run once with downloads on to cache them",
                    missing.join(", ")
                )?;
                match java {
                    Some(needed) => write!(f, ", or set JAVA_HOME to a Java {needed} or newer JDK"),
                    None => Ok(()),
                }
            }
            ProvisionError::Download { url, detail } => {
                write!(f, "could not download {url}: {detail}")
            }
            ProvisionError::Corrupt { url, detail } => write!(
                f,
                "the download from {url} is not the file krusty pins ({detail}); nothing was kept"
            ),
            ProvisionError::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for ProvisionError {}

/// Finds or fetches the JDK and the compiler classpath for `request`. Each download or copy is
/// announced on `report` with what it is and why; when everything is already in place, nothing is
/// printed.
pub fn provision(
    request: &Request,
    env: &Environment,
    pins: &Pins,
    fetcher: &dyn Fetcher,
    report: &mut dyn Write,
) -> Result<HostRuntime, ProvisionError> {
    let classpath_pins = pins
        .compiler_classpath(request.kotlin_version)
        .ok_or_else(|| ProvisionError::UnpinnedKotlin {
            version: request.kotlin_version.to_string(),
            pinned: pins.kotlin_versions(),
        })?;
    let mut session = Session {
        request,
        env,
        fetcher,
        report,
        missing: Vec::new(),
        missing_java: None,
        classpath_announced: false,
        changed: false,
    };
    let jdk = session.jdk(pins.jdk())?;
    let mut classpath = Vec::new();
    for pin in classpath_pins {
        classpath.extend(session.artifact(pin)?);
    }
    if !session.missing.is_empty() {
        return Err(ProvisionError::Offline {
            reason: request.reason.to_string(),
            missing: session.missing,
            java: session.missing_java,
        });
    }
    if session.changed {
        session.note(format_args!(
            "saved in {}; later builds reuse it",
            env.cache_dir.display()
        ));
    }
    let jdk = jdk.expect("a JDK, since nothing is missing");
    Ok(HostRuntime {
        kotlin_version: request.kotlin_version.to_string(),
        java: jdk.java,
        java_home: jdk.home,
        compiler_classpath: classpath,
        compiler_identity: ArtifactPin::identity(classpath_pins),
    })
}

struct Session<'a> {
    request: &'a Request<'a>,
    env: &'a Environment,
    fetcher: &'a dyn Fetcher,
    report: &'a mut dyn Write,
    missing: Vec<String>,
    missing_java: Option<u32>,
    classpath_announced: bool,
    changed: bool,
}

impl Session<'_> {
    fn note(&mut self, message: std::fmt::Arguments) {
        let _ = writeln!(self.report, "info: {message}");
    }

    /// An installed JDK when one is new enough, else the pinned one from the cache or a download.
    /// `None` only when it is missing and downloads are off.
    fn jdk(&mut self, pin: Option<&JdkPin>) -> Result<Option<Jdk>, ProvisionError> {
        let needed = self.request.min_java.max(COMPILER_MIN_JAVA);
        let platform = &self.env.platform;
        let mut rejected = Vec::new();
        let candidates = jdk::candidates(
            self.env.java_home.as_deref(),
            self.env.path.as_deref(),
            platform,
        );
        for (home, source) in candidates {
            match jdk::inspect(&home, platform) {
                Ok(jdk) if jdk.major >= needed => return Ok(Some(jdk)),
                Ok(jdk) => {
                    rejected.push(format!("{source} {} is Java {}", home.display(), jdk.major))
                }
                Err(why) => rejected.push(format!("{source}: {why}")),
            }
        }
        if rejected.is_empty() {
            rejected.push("JAVA_HOME is unset and no java is on PATH".to_string());
        }
        let platform_name = format!("{}-{}", platform.os, platform.arch);
        let Some(pin) = pin.filter(|pin| pin.major >= needed) else {
            return Err(ProvisionError::NoJdk {
                reason: self.request.reason.to_string(),
                needed,
                rejected,
                platform: platform_name,
            });
        };
        let home_dir = self
            .env
            .cache_dir
            .join("jdk")
            .join(format!("{}-{platform_name}", pin.release));
        let java = home_dir.join(&pin.java);
        if java.is_file() {
            return Ok(Some(jdk_at(java, pin.major)));
        }
        // A directory without the launcher is a broken earlier install; publishing beside it would
        // keep it, since a rename never replaces a non-empty directory.
        install::remove_entry(&home_dir).map_err(|error| io(&home_dir, error))?;
        if self.env.offline {
            self.missing.push(format!(
                "a Java {needed} or newer JDK ({})",
                rejections(&rejected)
            ));
            self.missing_java = Some(needed);
            return Ok(None);
        }
        self.note(format_args!(
            "{} runs in a JVM, but no installed JDK is Java {needed} or newer ({}); krusty downloads one",
            self.request.reason,
            rejections(&rejected)
        ));
        self.note(format_args!(
            "downloading the {} JDK for {platform_name} ({}) from {}",
            pin.release,
            file_size(pin.size),
            pin.url
        ));
        let jdk_dir = self.env.cache_dir.join("jdk");
        create_dir(&jdk_dir)?;
        let archive = install::staging_path(&jdk_dir.join(pin.file_name()));
        let fetched = self.fetch_verified(&pin.url, &archive, &pin.sha256, pin.size);
        let unpacked = fetched.and_then(|()| {
            let staged = install::staging_path(&home_dir);
            install::unpack(&archive, pin.file_name(), &staged).map_err(|detail| {
                ProvisionError::Io {
                    path: archive.clone(),
                    detail: format!("could not unpack: {detail}"),
                }
            })?;
            if !staged.join(&pin.java).is_file() {
                install::remove(&staged);
                return Err(ProvisionError::Io {
                    path: archive.clone(),
                    detail: format!("the archive has no {}", pin.java.display()),
                });
            }
            install::publish(&staged, &home_dir, |home| home.join(&pin.java).is_file())
                .map_err(|error| io(&home_dir, error))
        });
        install::remove(&archive);
        unpacked?;
        self.changed = true;
        Ok(Some(jdk_at(java, pin.major)))
    }

    /// The pinned jar in krusty's cache, put there from another build's cache or a download first
    /// when needed. `None` only when it is missing and downloads are off.
    fn artifact(&mut self, pin: &ArtifactPin) -> Result<Option<PathBuf>, ProvisionError> {
        let dest = artifacts::cached(self.env, pin);
        let valid = |jar: &std::path::Path| install::verify(jar, &pin.sha256, pin.size).is_ok();
        // The cache is checked like any other copy: the host runs these jars.
        if valid(&dest) {
            return Ok(Some(dest));
        }
        // Anything else at `dest` is not the pinned jar, and a rename on Windows never replaces it.
        install::remove_entry(&dest).map_err(|error| io(&dest, error))?;
        create_dir(dest.parent().expect("a cached jar is inside the cache"))?;
        for (copy, cache) in artifacts::installed_copies(self.env, pin) {
            let staged = install::staging_path(&dest);
            if std::fs::copy(&copy, &staged).is_err()
                || install::verify(&staged, &pin.sha256, pin.size).is_err()
            {
                install::remove(&staged);
                continue;
            }
            self.announce_classpath();
            self.note(format_args!(
                "copying {} from {cache} ({})",
                pin.file_name(),
                copy.display()
            ));
            install::publish(&staged, &dest, valid).map_err(|error| io(&dest, error))?;
            self.changed = true;
            return Ok(Some(dest));
        }
        if self.env.offline {
            self.missing.push(pin.file_name());
            return Ok(None);
        }
        self.announce_classpath();
        let url = artifacts::url(self.env, pin);
        self.note(format_args!(
            "downloading {} ({}) from {url}",
            pin.file_name(),
            file_size(pin.size)
        ));
        let staged = install::staging_path(&dest);
        self.fetch_verified(&url, &staged, &pin.sha256, pin.size)?;
        install::publish(&staged, &dest, valid).map_err(|error| io(&dest, error))?;
        self.changed = true;
        Ok(Some(dest))
    }

    fn announce_classpath(&mut self) {
        if !self.classpath_announced {
            self.classpath_announced = true;
            self.note(format_args!(
                "{} runs on kotlinc's own compiler, kotlin-compiler-embeddable {}, which krusty keeps \
                 in its cache",
                self.request.reason, self.request.kotlin_version
            ));
        }
    }

    /// Downloads `url` to `dest` and checks it against the pin, leaving nothing behind otherwise.
    fn fetch_verified(
        &self,
        url: &str,
        dest: &std::path::Path,
        sha256: &str,
        size: u64,
    ) -> Result<(), ProvisionError> {
        if let Err(detail) = self.fetcher.fetch(url, dest) {
            install::remove(dest);
            return Err(ProvisionError::Download {
                url: url.to_string(),
                detail,
            });
        }
        let detail = match install::verify(dest, sha256, size) {
            Ok(()) => return Ok(()),
            Err(Mismatch::Size { actual }) => format!("{actual} bytes, expected {size}"),
            Err(Mismatch::Digest { actual }) => format!("SHA-256 {actual}, expected {sha256}"),
            Err(Mismatch::Unreadable(error)) => error.to_string(),
        };
        install::remove(dest);
        Err(ProvisionError::Corrupt {
            url: url.to_string(),
            detail,
        })
    }
}

/// The JDK whose launcher is `java`, at `<home>/bin/java`.
fn jdk_at(java: PathBuf, major: u32) -> Jdk {
    let home = java
        .parent()
        .and_then(|bin| bin.parent())
        .expect("a pinned launcher is <home>/bin/java")
        .to_path_buf();
    Jdk { home, java, major }
}

fn rejections(rejected: &[String]) -> String {
    rejected.join("; ")
}

fn file_size(size: u64) -> String {
    match size {
        0..1_000 => format!("{size} bytes"),
        1_000..1_000_000 => format!("{} KB", size.div_ceil(1_000)),
        _ => format!("{:.1} MB", size as f64 / 1_000_000.0),
    }
}

fn create_dir(dir: &std::path::Path) -> Result<(), ProvisionError> {
    std::fs::create_dir_all(dir).map_err(|error| io(dir, error))
}

fn io(path: &std::path::Path, error: std::io::Error) -> ProvisionError {
    ProvisionError::Io {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}
