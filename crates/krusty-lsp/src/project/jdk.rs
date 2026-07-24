//! JDK discovery and validation.

use std::path::{Path, PathBuf};

use super::runner::{Command, CommandRunner};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JdkSource {
    /// `-jdk-home` on the command line.
    Explicit,
    /// A toolchain reported by the project provider.
    Toolchain,
    JavaHome,
    /// macOS `/usr/libexec/java_home`.
    SystemHelper,
    /// `java` resolved through `PATH`.
    Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jdk {
    pub home: PathBuf,
    pub source: JdkSource,
}

/// Filesystem and environment access, injected so the resolution order is testable.
pub trait JdkEnvironment {
    fn var(&self, name: &str) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
    /// Resolve symlinks; `PATH` entries for `java` are usually shims.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
    fn is_macos(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

#[derive(Debug, Default)]
pub struct SystemEnvironment;

impl JdkEnvironment for SystemEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

/// What a JDK home must contain for the compiler to read the platform classes from it.
pub fn is_jdk_home(environment: &dyn JdkEnvironment, home: &Path) -> bool {
    environment.is_file(&home.join("lib").join("modules"))
}

#[derive(Debug, Default)]
pub struct JdkRequest<'a> {
    pub explicit: Option<&'a Path>,
    pub toolchain: Option<&'a Path>,
    /// Passed to `/usr/libexec/java_home -v` when known.
    pub jvm_target: Option<&'a str>,
}

pub fn resolve_jdk(
    environment: &dyn JdkEnvironment,
    runner: &dyn CommandRunner,
    request: &JdkRequest<'_>,
) -> Option<Jdk> {
    let candidates: [(Option<PathBuf>, JdkSource); 3] = [
        (request.explicit.map(Path::to_path_buf), JdkSource::Explicit),
        (
            request.toolchain.map(Path::to_path_buf),
            JdkSource::Toolchain,
        ),
        (
            environment.var("JAVA_HOME").map(PathBuf::from),
            JdkSource::JavaHome,
        ),
    ];
    for (home, source) in candidates {
        if let Some(home) = home.filter(|home| is_jdk_home(environment, home)) {
            return Some(Jdk { home, source });
        }
    }
    if let Some(home) = system_helper_home(environment, runner, request.jvm_target) {
        return Some(Jdk {
            home,
            source: JdkSource::SystemHelper,
        });
    }
    java_on_path(environment).map(|home| Jdk {
        home,
        source: JdkSource::Path,
    })
}

fn system_helper_home(
    environment: &dyn JdkEnvironment,
    runner: &dyn CommandRunner,
    jvm_target: Option<&str>,
) -> Option<PathBuf> {
    if !environment.is_macos() {
        return None;
    }
    let helper = Path::new("/usr/libexec/java_home");
    if !environment.is_file(helper) {
        return None;
    }
    let mut command = Command::new(helper, "/");
    if let Some(version) = jvm_target {
        command = command.args(["-v".to_string(), version.to_string()]);
    }
    let output = runner.run(&command).ok()?;
    if !output.succeeded() {
        return None;
    }
    let home = PathBuf::from(output.stdout.trim());
    is_jdk_home(environment, &home).then_some(home)
}

fn java_on_path(environment: &dyn JdkEnvironment) -> Option<PathBuf> {
    let path = environment.var("PATH")?;
    for directory in std::env::split_paths(&path) {
        let executable = directory.join(if cfg!(windows) { "java.exe" } else { "java" });
        if !environment.is_file(&executable) {
            continue;
        }
        let resolved = environment
            .canonicalize(&executable)
            .unwrap_or_else(|| executable.clone());
        let home = resolved.parent().and_then(Path::parent)?;
        if is_jdk_home(environment, home) {
            return Some(home.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::project::runner::testing::FakeRunner;

    #[derive(Default)]
    struct FakeEnvironment {
        vars: HashMap<String, String>,
        files: HashSet<PathBuf>,
        links: HashMap<PathBuf, PathBuf>,
        macos: bool,
    }

    impl FakeEnvironment {
        fn with_jdk(mut self, home: &str) -> Self {
            self.files
                .insert(PathBuf::from(home).join("lib").join("modules"));
            self
        }

        fn with_var(mut self, name: &str, value: &str) -> Self {
            self.vars.insert(name.to_string(), value.to_string());
            self
        }

        fn with_file(mut self, path: &str) -> Self {
            self.files.insert(PathBuf::from(path));
            self
        }

        fn with_link(mut self, from: &str, to: &str) -> Self {
            self.links.insert(PathBuf::from(from), PathBuf::from(to));
            self
        }

        fn macos(mut self) -> Self {
            self.macos = true;
            self
        }
    }

    impl JdkEnvironment for FakeEnvironment {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }

        fn is_file(&self, path: &Path) -> bool {
            self.files.contains(path)
        }

        fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
            self.links.get(path).cloned()
        }

        fn is_macos(&self) -> bool {
            self.macos
        }
    }

    #[test]
    fn the_explicit_home_wins_over_the_toolchain_and_the_environment() {
        let environment = FakeEnvironment::default()
            .with_jdk("/jdk-explicit")
            .with_jdk("/jdk-toolchain")
            .with_jdk("/jdk-env")
            .with_var("JAVA_HOME", "/jdk-env");
        let jdk = resolve_jdk(
            &environment,
            &FakeRunner::default(),
            &JdkRequest {
                explicit: Some(Path::new("/jdk-explicit")),
                toolchain: Some(Path::new("/jdk-toolchain")),
                jvm_target: None,
            },
        )
        .unwrap();
        assert_eq!(jdk.home, PathBuf::from("/jdk-explicit"));
        assert_eq!(jdk.source, JdkSource::Explicit);
    }

    #[test]
    fn a_home_without_lib_modules_is_skipped_rather_than_trusted() {
        let environment = FakeEnvironment::default()
            .with_jdk("/jdk-toolchain")
            .with_var("JAVA_HOME", "/not-a-jdk");
        let jdk = resolve_jdk(
            &environment,
            &FakeRunner::default(),
            &JdkRequest {
                explicit: Some(Path::new("/also-not-a-jdk")),
                toolchain: Some(Path::new("/jdk-toolchain")),
                jvm_target: None,
            },
        )
        .unwrap();
        assert_eq!(jdk.source, JdkSource::Toolchain);
    }

    #[test]
    fn macos_falls_back_to_java_home_helper_with_the_requested_version() {
        let environment = FakeEnvironment::default()
            .macos()
            .with_file("/usr/libexec/java_home")
            .with_jdk("/Library/Java/JavaVirtualMachines/jdk-21.jdk/Contents/Home");
        let runner = FakeRunner::new(vec![FakeRunner::stdout(
            "/Library/Java/JavaVirtualMachines/jdk-21.jdk/Contents/Home\n",
        )]);
        let jdk = resolve_jdk(
            &environment,
            &runner,
            &JdkRequest {
                jvm_target: Some("21"),
                ..JdkRequest::default()
            },
        )
        .unwrap();
        assert_eq!(jdk.source, JdkSource::SystemHelper);
        assert_eq!(runner.command(0).args, vec!["-v", "21"]);
    }

    #[test]
    fn path_lookup_resolves_the_shim_symlink_before_taking_the_home() {
        let environment = FakeEnvironment::default()
            .with_var("PATH", "/usr/bin")
            .with_file("/usr/bin/java")
            .with_link("/usr/bin/java", "/opt/jdk21/bin/java")
            .with_jdk("/opt/jdk21");
        let jdk =
            resolve_jdk(&environment, &FakeRunner::default(), &JdkRequest::default()).unwrap();
        assert_eq!(jdk.home, PathBuf::from("/opt/jdk21"));
        assert_eq!(jdk.source, JdkSource::Path);
    }

    #[test]
    fn no_jdk_anywhere_is_reported_as_none_rather_than_a_guess() {
        let environment = FakeEnvironment::default().with_var("PATH", "/usr/bin");
        assert_eq!(
            resolve_jdk(&environment, &FakeRunner::default(), &JdkRequest::default()),
            None
        );
    }
}
