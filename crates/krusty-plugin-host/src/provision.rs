//! Finding or fetching the JDK and compiler jars one host runs on, telling the user what is fetched
//! and why.

use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// The feature release of [`Self::java`], as its home's `release` file names it.
    pub java_major: u32,
    /// `kotlin-compiler-embeddable` first, then its runtime dependencies.
    pub compiler_classpath: Vec<PathBuf>,
    /// [`ArtifactPin::identity`] of the pins the classpath was verified against, which names what is
    /// built against it, such as the host's adapter.
    pub compiler_identity: String,
}

impl HostRuntime {
    /// A command that runs this runtime's `java` as its own JDK: `JAVA_HOME` names it, whatever the
    /// environment named, so what runs there and looks for a JDK finds the one that runs it.
    ///
    /// From Java 9 the JVM's own logging writes its warnings to stdout, where the host speaks its
    /// protocol, and `JDK_JAVA_OPTIONS` can add more logging there. The command turns every
    /// configured log output off and sends warnings and errors to stderr instead. Java 8 has no
    /// such logging and no such option.
    pub(crate) fn java_command(&self) -> std::process::Command {
        java_command(&self.java, &self.java_home, self.java_major)
    }
}

fn java_command(java: &Path, java_home: &Path, java_major: u32) -> std::process::Command {
    let mut command = std::process::Command::new(java);
    command.env("JAVA_HOME", java_home);
    if java_major >= 9 {
        command.args(["-Xlog:disable", "-Xlog:all=warning:stderr"]);
    }
    command
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProvisionError {
    /// No compiler classpath is pinned for this exact Kotlin version.
    UnpinnedKotlin {
        version: String,
        pinned: Vec<String>,
    },
    /// No API classpath is pinned for this exact KSP version.
    UnpinnedKsp {
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
            ProvisionError::UnpinnedKsp { version, pinned } => write!(
                f,
                "krusty cannot host KSP {version} processors: it has no pinned \
                 symbol-processing-api classpath for that exact version (pinned: {})",
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
    let preface = format!(
        "{} runs on kotlinc's own compiler, kotlin-compiler-embeddable {}, which krusty keeps \
         in its cache",
        request.reason, request.kotlin_version
    );
    let wanted = Wanted {
        reason: request.reason,
        min_java: request.min_java,
        preface,
        classpath: classpath_pins,
    };
    let (jdk, classpath) = jdk_and_classpath(wanted, env, pins, fetcher, report)?;
    Ok(HostRuntime {
        kotlin_version: request.kotlin_version.to_string(),
        java: jdk.java,
        java_home: jdk.home,
        java_major: jdk.major,
        compiler_classpath: classpath,
        compiler_identity: ArtifactPin::identity(classpath_pins),
    })
}

/// What a KSP host is provisioned for.
pub struct KspRequest<'a> {
    /// The build's KSP version, exactly as configured.
    pub ksp_version: &'a str,
    /// What needs the host, as the user should read it, such as `the KSP processors of module app`.
    pub reason: &'a str,
    /// The oldest Java the processors can run on. Below [`COMPILER_MIN_JAVA`] it is raised to it.
    pub min_java: u32,
}

/// What a KSP host process is started with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspRuntime {
    /// The KSP version the API classpath is, exactly as the build configured it.
    pub ksp_version: String,
    pub java: PathBuf,
    pub java_home: PathBuf,
    /// The feature release of [`Self::java`], as its home's `release` file names it.
    pub java_major: u32,
    /// KSP's `symbol-processing-api` first, then the standard library it runs on.
    pub api_classpath: Vec<PathBuf>,
    /// [`ArtifactPin::identity`] of the pins the API classpath was verified against.
    pub api_identity: String,
}

impl KspRuntime {
    /// A command that runs this runtime's `java`, as [`HostRuntime::java_command`] does.
    pub(crate) fn java_command(&self) -> std::process::Command {
        java_command(&self.java, &self.java_home, self.java_major)
    }
}

/// Finds or fetches the JDK and KSP's API classpath for `request`, announcing what is fetched as
/// [`provision`] does.
pub fn provision_ksp(
    request: &KspRequest,
    env: &Environment,
    pins: &Pins,
    fetcher: &dyn Fetcher,
    report: &mut dyn Write,
) -> Result<KspRuntime, ProvisionError> {
    let classpath_pins =
        pins.ksp_classpath(request.ksp_version)
            .ok_or_else(|| ProvisionError::UnpinnedKsp {
                version: request.ksp_version.to_string(),
                pinned: pins.ksp_versions(),
            })?;
    let preface = format!(
        "{} runs on KSP's API, symbol-processing-api {}, which krusty keeps in its cache",
        request.reason, request.ksp_version
    );
    let wanted = Wanted {
        reason: request.reason,
        min_java: request.min_java,
        preface,
        classpath: classpath_pins,
    };
    let (jdk, classpath) = jdk_and_classpath(wanted, env, pins, fetcher, report)?;
    Ok(KspRuntime {
        ksp_version: request.ksp_version.to_string(),
        java: jdk.java,
        java_home: jdk.home,
        java_major: jdk.major,
        api_classpath: classpath,
        api_identity: ArtifactPin::identity(classpath_pins),
    })
}

/// What one host runs on: a JDK and a pinned classpath.
struct Wanted<'a> {
    /// What needs them, as the user should read it.
    reason: &'a str,
    min_java: u32,
    /// Said once, before the first jar is copied or downloaded.
    preface: String,
    classpath: &'a [ArtifactPin],
}

/// The JDK and the jars `wanted` names, found or fetched.
fn jdk_and_classpath(
    wanted: Wanted,
    env: &Environment,
    pins: &Pins,
    fetcher: &dyn Fetcher,
    report: &mut dyn Write,
) -> Result<(Jdk, Vec<PathBuf>), ProvisionError> {
    let reason = wanted.reason;
    let mut session = Session {
        reason,
        min_java: wanted.min_java,
        env,
        fetcher,
        report,
        missing: Vec::new(),
        missing_java: None,
        preface: Some(wanted.preface),
        changed: false,
    };
    let jdk = session.jdk(pins.jdk())?;
    let mut classpath = Vec::new();
    for pin in wanted.classpath {
        classpath.extend(session.artifact(pin)?);
    }
    if !session.missing.is_empty() {
        return Err(ProvisionError::Offline {
            reason: reason.to_string(),
            missing: session.missing,
            java: session.missing_java,
        });
    }
    session.note_saved();
    Ok((jdk.expect("a JDK, since nothing is missing"), classpath))
}

/// Finds or fetches one pinned jar the way [`provision`] finds the compiler's: krusty's cache, else a
/// verified copy from the Gradle or Maven local cache, else a verified download, announced on
/// `report` with `reason`, such as `the compiler plugin power-assert`, which needs it.
pub fn provision_artifact(
    pin: &ArtifactPin,
    reason: &str,
    env: &Environment,
    fetcher: &dyn Fetcher,
    report: &mut dyn Write,
) -> Result<PathBuf, ProvisionError> {
    let mut session = Session {
        reason,
        min_java: COMPILER_MIN_JAVA,
        env,
        fetcher,
        report,
        missing: Vec::new(),
        missing_java: None,
        preface: Some(format!(
            "{reason} needs {}, which krusty keeps in its cache",
            pin.file_name()
        )),
        changed: false,
    };
    let Some(jar) = session.artifact(pin)? else {
        return Err(ProvisionError::Offline {
            reason: reason.to_string(),
            missing: session.missing,
            java: None,
        });
    };
    session.note_saved();
    Ok(jar)
}

struct Session<'a> {
    /// What needs the files, as the user should read it.
    reason: &'a str,
    min_java: u32,
    env: &'a Environment,
    fetcher: &'a dyn Fetcher,
    report: &'a mut dyn Write,
    missing: Vec<String>,
    missing_java: Option<u32>,
    /// Said once, before the first jar is copied or downloaded.
    preface: Option<String>,
    changed: bool,
}

impl Session<'_> {
    fn note(&mut self, message: std::fmt::Arguments) {
        let _ = writeln!(self.report, "info: {message}");
    }

    fn note_saved(&mut self) {
        if self.changed {
            let cache = self.env.cache_dir.display().to_string();
            self.note(format_args!("saved in {cache}; later builds reuse it"));
        }
    }

    /// An installed JDK when one is new enough, else the pinned one from the cache or a download.
    /// `None` only when it is missing and downloads are off.
    fn jdk(&mut self, pin: Option<&JdkPin>) -> Result<Option<Jdk>, ProvisionError> {
        let needed = self.min_java.max(COMPILER_MIN_JAVA);
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
                reason: self.reason.to_string(),
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
        let reason = self.reason;
        self.note(format_args!(
            "{reason} runs in a JVM, but no installed JDK is Java {needed} or newer ({}); krusty downloads one",
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
            self.announce();
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
        self.announce();
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

    fn announce(&mut self) {
        if let Some(preface) = self.preface.take() {
            self.note(format_args!("{preface}"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(major: u32) -> HostRuntime {
        HostRuntime {
            kotlin_version: "2.4.20".into(),
            java: PathBuf::from(format!("/jdks/{major}/bin/java")),
            java_home: PathBuf::from(format!("/jdks/{major}")),
            java_major: major,
            compiler_classpath: Vec::new(),
            compiler_identity: String::new(),
        }
    }

    #[test]
    fn java_runs_with_its_own_home_as_java_home() {
        let command = runtime(21).java_command();
        assert_eq!(command.get_program(), "/jdks/21/bin/java");
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            [(
                std::ffi::OsStr::new("JAVA_HOME"),
                Some(std::ffi::OsStr::new("/jdks/21"))
            )]
        );
    }

    #[test]
    fn java_9_and_later_log_to_stderr_and_java_8_takes_no_logging_option() {
        let arguments = |major| {
            runtime(major)
                .java_command()
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(arguments(9), ["-Xlog:disable", "-Xlog:all=warning:stderr"]);
        assert_eq!(arguments(21), ["-Xlog:disable", "-Xlog:all=warning:stderr"]);
        assert_eq!(arguments(8), Vec::<String>::new());
    }
}
