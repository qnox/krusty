//! Compile a Gradle build by running `./gradlew` with the Kotlin JVM plugin pointed at krusty.
//!
//! The Kotlin Gradle plugin has no JVM compiler-executable property. A build applies plugin id
//! `krusty` beside `org.jetbrains.kotlin.jvm`; the Kotlin DSL stays. [`repository_plugin_project`]
//! is the source plugin, included with `--include-build` for checkouts or resolved from the release
//! Maven repository. It attaches a typed task which execs krusty with the complete sources,
//! classpaths, friend paths, and supported structured options. The plugin does not call this
//! crate. kotlinc and the Kotlin compile daemon are not started.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Failure while launching Gradle.
#[derive(Debug)]
pub enum GradleError {
    Launch(String),
    Failed { status: String, message: String },
}

impl std::fmt::Display for GradleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(message) => f.write_str(message),
            Self::Failed { status, message } => write!(f, "gradle {status}: {message}"),
        }
    }
}

/// The Gradle plugin project shipped next to this crate. Callers pass it to [`GradleBuild`].
pub fn repository_plugin_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/krusty-gradle")
}

/// One `./gradlew` invocation whose Kotlin compiles exec krusty.
#[derive(Clone, Debug)]
pub struct GradleBuild {
    root: PathBuf,
    plugin_project: PathBuf,
    plugin_repository: Option<PathBuf>,
    gradle: PathBuf,
    krusty_binary: PathBuf,
    tasks: Vec<String>,
    properties: Vec<(String, String)>,
    no_daemon: bool,
    configuration_cache: bool,
}

impl GradleBuild {
    /// `root` is the Gradle build. It applies plugin id `krusty`. `plugin_project` is
    /// [`repository_plugin_project`] when the caller is this repository. `krusty_binary` is the
    /// compiler executable the Kotlin compile tasks run.
    pub fn new(
        root: impl Into<PathBuf>,
        plugin_project: impl Into<PathBuf>,
        krusty_binary: impl Into<PathBuf>,
    ) -> Self {
        let root = root.into();
        let gradle = gradle_launcher(&root);
        Self {
            root,
            plugin_project: plugin_project.into(),
            plugin_repository: None,
            gradle,
            krusty_binary: krusty_binary.into(),
            tasks: vec!["krustyCompile".to_string()],
            properties: Vec::new(),
            no_daemon: false,
            configuration_cache: false,
        }
    }

    pub fn no_daemon(mut self, no_daemon: bool) -> Self {
        self.no_daemon = no_daemon;
        self
    }

    pub fn configuration_cache(mut self, enabled: bool) -> Self {
        self.configuration_cache = enabled;
        self
    }

    /// Replace the default `krustyCompile` task. Gradle still orders project dependencies.
    pub fn tasks<I, S>(mut self, tasks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tasks = tasks.into_iter().map(Into::into).collect();
        self
    }

    pub fn property(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.properties.push((name.into(), value.into()));
        self
    }

    /// Resolve the plugin from a portable local Maven repository. Release validation uses this to
    /// execute the exact bytes attached to GitHub releases.
    pub fn plugin_repository(mut self, repository: impl Into<PathBuf>) -> Self {
        self.plugin_repository = Some(repository.into());
        self
    }

    /// Arguments passed to `./gradlew`. `--include-build` supplies the `krusty` plugin. This crate
    /// is not on that command line.
    pub fn arguments(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.no_daemon {
            args.push("--no-daemon".to_string());
        }
        if self.configuration_cache {
            args.push("--configuration-cache".to_string());
        }
        args.push("--console=plain".to_string());
        if let Some(repository) = &self.plugin_repository {
            args.push(format!(
                "-Pkrusty.plugin.repository={}",
                repository.display()
            ));
        } else {
            args.push("--include-build".to_string());
            args.push(self.plugin_project.display().to_string());
        }
        args.push(format!("-Pkrusty.binary={}", self.krusty_binary.display()));
        args.extend(
            self.properties
                .iter()
                .map(|(name, value)| format!("-P{name}={value}")),
        );
        args.extend(self.tasks.iter().cloned());
        args
    }

    /// Run Gradle. Kotlin compilation is done inside that build by execing the krusty binary.
    pub fn run(&self) -> Result<(), GradleError> {
        self.run_output().map(|_| ())
    }

    /// Run Gradle and return its combined diagnostic output. Integration tests use this to assert
    /// configuration-cache reuse rather than merely observing a successful second invocation.
    pub fn run_output(&self) -> Result<String, GradleError> {
        if !self.gradle.is_file() {
            return Err(GradleError::Launch(format!(
                "gradle wrapper not found at {}",
                self.gradle.display()
            )));
        }
        match &self.plugin_repository {
            Some(repository) if !repository.is_dir() => {
                return Err(GradleError::Launch(format!(
                    "gradle plugin repository not found at {}",
                    repository.display()
                )))
            }
            None if !self.plugin_project.join("settings.gradle.kts").is_file() => {
                return Err(GradleError::Launch(format!(
                    "gradle plugin project not found at {}",
                    self.plugin_project.display()
                )))
            }
            _ => {}
        }
        if !self.krusty_binary.is_file() {
            return Err(GradleError::Launch(format!(
                "krusty binary not found at {}",
                self.krusty_binary.display()
            )));
        }
        let output = Command::new(&self.gradle)
            .current_dir(&self.root)
            .args(self.arguments())
            .output()
            .map_err(|error| GradleError::Launch(format!("{}: {error}", self.gradle.display())))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(GradleError::Failed {
                status: output.status.to_string(),
                message: diagnostic_tails(&stderr, &stdout),
            });
        }
        Ok(format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ))
    }
}

fn gradle_launcher(root: &Path) -> PathBuf {
    let unix = root.join("gradlew");
    if unix.is_file() {
        unix
    } else {
        root.join("gradlew.bat")
    }
}

fn tail(text: &str) -> String {
    const MAX_LINES: usize = 40;
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

fn diagnostic_tails(stderr: &str, stdout: &str) -> String {
    format!(
        "--- stderr (tail) ---\n{}\n--- stdout (tail) ---\n{}",
        tail(stderr),
        tail(stdout),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_gradle_output_keeps_stderr_and_stdout_tails_separate() {
        let stderr = (0..50)
            .map(|line| format!("stderr {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let stdout = (0..50)
            .map(|line| format!("stdout {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let message = diagnostic_tails(&stderr, &stdout);
        let lines = message.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 82);
        assert_eq!(lines[0], "--- stderr (tail) ---");
        assert_eq!(lines[1], "stderr 10");
        assert_eq!(lines[40], "stderr 49");
        assert_eq!(lines[41], "--- stdout (tail) ---");
        assert_eq!(lines[42], "stdout 10");
        assert_eq!(lines[81], "stdout 49");
    }

    #[test]
    fn gradle_arguments_exec_krusty_and_do_not_launch_this_crate() {
        let build =
            GradleBuild::new("/kotlin", repository_plugin_project(), "/opt/krusty").no_daemon(true);
        let args = build.arguments();
        assert!(args.iter().any(|arg| arg == "--no-daemon"));
        assert!(args.iter().all(|arg| arg != "--no-configuration-cache"));
        let included = args.windows(2).find(|pair| pair[0] == "--include-build");
        let Some(pair) = included else {
            panic!("missing --include-build in {args:?}");
        };
        assert!(pair[1].ends_with("tools/krusty-gradle"));
        assert!(args.iter().all(|arg| arg != "--init-script"));
        assert!(args.iter().any(|arg| arg == "-Pkrusty.binary=/opt/krusty"));
        assert_eq!(args.last().map(String::as_str), Some("krustyCompile"));
        assert!(args.iter().all(|arg| !arg.contains("krusty.graph.out")));
        assert!(args.iter().all(|arg| arg != "krustyGraph"));
        assert!(args.iter().all(|arg| {
            Path::new(arg).file_name().and_then(|name| name.to_str()) != Some("krusty-build")
        }));

        let one = build
            .tasks([":compiler:util:compileKotlin"])
            .configuration_cache(true)
            .property("kotlinPluginVersion", "2.4.10");
        assert!(one
            .arguments()
            .iter()
            .any(|arg| arg == "--configuration-cache"));
        assert_eq!(
            one.arguments().last().map(String::as_str),
            Some(":compiler:util:compileKotlin")
        );
        let released = one.plugin_repository("/opt/krusty-plugin/repository");
        assert!(released
            .arguments()
            .iter()
            .all(|arg| arg != "--include-build"));
        assert!(released
            .arguments()
            .iter()
            .any(|arg| { arg == "-Pkrusty.plugin.repository=/opt/krusty-plugin/repository" }));
    }

    #[test]
    fn run_reports_a_missing_wrapper_without_running_gradle() {
        let missing = std::env::temp_dir().join(format!("krusty-no-gradle-{}", std::process::id()));
        let error = GradleBuild::new(&missing, repository_plugin_project(), "/opt/krusty")
            .run()
            .expect_err("missing wrapper");
        assert!(
            error.to_string().contains("gradle wrapper not found"),
            "{error}"
        );
    }

    #[test]
    fn supported_gradle_kotlin_versions_follow_the_repository_manifest() {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest = std::fs::read_to_string(repository.join("kotlin-versions"))
            .expect("read kotlin-versions");
        let expected = manifest
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.split_whitespace().next().expect("reference version"))
            .collect::<Vec<_>>();

        let plugin = std::fs::read_to_string(
            repository.join("tools/krusty-gradle/src/main/kotlin/krusty/KrustyKotlinPlugin.kt"),
        )
        .expect("read Gradle plugin");
        let supported = plugin
            .split_once("private fun supportedKotlinPluginVersion")
            .expect("supported Kotlin plugin versions")
            .1
            .split_once("else ->")
            .expect("unsupported-version arm")
            .0;
        let plugin_versions = quoted_release_versions(supported);
        assert_eq!(plugin_versions, expected);

        let packaging =
            std::fs::read_to_string(repository.join("scripts/package-gradle-plugin.sh"))
                .expect("read Gradle packaging script");
        let documented_versions = packaging
            .lines()
            .find_map(|line| {
                line.split_once("// supported: ")
                    .map(|(_, versions)| versions)
            })
            .expect("packaged README supported versions")
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>();
        assert_eq!(documented_versions, expected);
        let manifest_versions = packaging
            .lines()
            .find_map(|line| line.strip_prefix("kotlin-gradle-plugin="))
            .expect("packaged manifest supported versions")
            .split(',')
            .collect::<Vec<_>>();
        assert_eq!(manifest_versions, expected);

        let workflow = std::fs::read_to_string(repository.join(".github/workflows/ci.yml"))
            .expect("read CI workflow");
        let mut matrix_versions = workflow
            .lines()
            .filter_map(|line| line.split_once("{ kotlin:").map(|(_, row)| row))
            .filter_map(|row| row.split(',').next())
            .map(|version| version.trim().trim_matches('\''))
            .collect::<Vec<_>>();
        matrix_versions.sort_unstable();
        matrix_versions.dedup();
        assert_eq!(matrix_versions, expected);
    }

    /// The release Maven repository is exercised with the real krusty CLI. The proxy records each
    /// command before forwarding it, so the fixture verifies both Gradle wiring and emitted bytes.
    #[test]
    #[ignore = "downloads Gradle and the Kotlin Gradle plugin"]
    fn kotlin_compiler_slice_compiles_through_krusty() {
        let gradle_bin = ensure_gradle();
        let root = std::env::temp_dir().join(format!(
            "krusty-kotlin-slice-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let plugin_version =
            std::env::var("KRUSTY_GRADLE_PLUGIN_VERSION").unwrap_or_else(|_| "0.0.1".into());
        let kgp = std::env::var("KRUSTY_GRADLE_KGP_VERSION").unwrap_or_else(|_| "2.4.20".into());
        write_compiler_slice(&root, &plugin_version, &kgp);
        // Gradle resolves the project directory through symlinks (macOS /var → /private/var);
        // the recorded invocations carry the resolved path, so the normalization root must too.
        let root = root.canonicalize().expect("canonicalize slice root");
        let log = root.join("krusty-invocations.txt");
        let proxy = root.join("recording-krusty");
        let actual = std::env::var_os("KRUSTY_GRADLE_TEST_BIN")
            .map(PathBuf::from)
            .expect("KRUSTY_GRADLE_TEST_BIN must name the built krusty CLI");
        write_krusty_proxy(&proxy, &log, &actual);
        let wrapper = root.join("gradlew");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec '{}' --build-cache \"$@\"\n",
                gradle_bin.display()
            ),
        )
        .expect("write gradlew");
        use std::os::unix::fs::PermissionsExt;
        let executable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&wrapper, executable.clone()).expect("chmod gradlew");
        std::fs::set_permissions(&proxy, executable).expect("chmod recording krusty");

        // Keep CI's mutable Gradle workspace cache outside Cargo's target directory. The Rust
        // cache action archives target as build output and may prune non-Cargo files from it,
        // leaving Kotlin DSL workspaces without their metadata on the next restore. Local runs
        // retain the repository cache unless the harness supplies an isolated home.
        let user_home = std::env::var_os("KRUSTY_GRADLE_USER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cache/gradle-user-home")
            });
        std::fs::create_dir_all(&user_home).expect("gradle user home");
        std::env::set_var("GRADLE_USER_HOME", &user_home);

        let plugin_project = std::env::var_os("KRUSTY_GRADLE_PLUGIN_PROJECT")
            .map(PathBuf::from)
            .unwrap_or_else(repository_plugin_project);
        let build = || {
            let build = GradleBuild::new(&root, repository_plugin_project(), &proxy)
                .property("kotlinPluginVersion", &kgp)
                .property("krustyPluginVersion", &plugin_version)
                .configuration_cache(true)
                .no_daemon(true);
            if plugin_project.join("repository").is_dir() {
                build.plugin_repository(plugin_project.join("repository"))
            } else {
                build
            }
        };
        let full_build_tasks = [
            ":core:util.runtime:classes",
            ":compiler:util:classes",
            ":compiler:util:testClasses",
        ];
        let run_all = || build().tasks(full_build_tasks).run();
        let run_all_output = || build().tasks(full_build_tasks).run_output();
        let run_util = || build().tasks([":compiler:util:compileKotlin"]).run();
        run_all().unwrap_or_else(|error| panic!("{error}"));

        let text = std::fs::read_to_string(&log).expect("invocation log");
        let runs = invocations(&text);
        assert_eq!(runs.len(), 3, "{text}");
        let runtime_at = runs
            .iter()
            .position(|args| args.iter().any(|arg| arg.ends_with("Runtime.kt")))
            .expect("runtime");
        let util_at = runs
            .iter()
            .position(|args| args.iter().any(|arg| arg.ends_with("Util.kt")))
            .expect("util");
        let friend_at = runs
            .iter()
            .position(|args| args.iter().any(|arg| arg.ends_with("Friend.kt")))
            .expect("friend");
        assert!(runtime_at < util_at);
        assert!(util_at < friend_at);
        let reference_version = format!("-Xkotlin-reference-version={kgp}");
        let runtime_classpath = normalized_path_list(&["$KOTLIN_STDLIB", "$ANNOTATIONS"]);
        let util_classpath = normalized_path_list(&[
            "$ROOT/core/util.runtime/build/classes/java/main",
            "$ROOT/core/util.runtime/build/classes/kotlin/main",
            "$KOTLIN_STDLIB",
            "$ANNOTATIONS",
        ]);
        let friend_classpath = normalized_path_list(&[
            "$ROOT/compiler/util/build/classes/java/main",
            "$ROOT/compiler/util/build/classes/kotlin/main",
            "$ROOT/compiler/util/build/resources/main",
            "$ROOT/core/util.runtime/build/classes/java/main",
            "$ROOT/core/util.runtime/build/classes/kotlin/main",
            "$KOTLIN_STDLIB",
            "$ANNOTATIONS",
        ]);
        let friend_paths = normalized_path_list(&[
            "$ROOT/compiler/util/build/classes/java/main",
            "$ROOT/compiler/util/build/classes/kotlin/main",
            "$ROOT/compiler/util/build/libs/util.jar",
        ]);
        let friend_argument = format!("-Xfriend-paths={friend_paths}");

        assert_eq!(
            normalized_invocation(&runs[runtime_at], &root, &kgp),
            strings(&[
                "$ROOT/core/util.runtime/src/org/jetbrains/kotlin/util/Runtime.kt",
                "$ROOT/core/util.runtime/src/org/jetbrains/kotlin/util/RuntimeJava.java",
                "-classpath",
                runtime_classpath.as_str(),
                "-Xconsistent-data-class-copy-visibility",
                "-language-version",
                "2.4",
                "-api-version",
                "2.4",
                "-module-name",
                "kotlin-util-runtime",
                "-jvm-target",
                "17",
                "-jvm-default",
                "no-compatibility",
                "-java-parameters",
                reference_version.as_str(),
                "-jdk-home",
                "$JDK_HOME",
                "-no-stdlib",
                "-no-reflect",
                "-d",
                "$ROOT/core/util.runtime/build/classes/kotlin/main",
            ])
        );
        assert_eq!(
            normalized_invocation(&runs[util_at], &root, &kgp),
            strings(&[
                "$ROOT/compiler/util/build/generated/krusty/org/jetbrains/kotlin/util/Generated.kt",
                "$ROOT/compiler/util/other-src/second/Same.kt",
                "$ROOT/compiler/util/src/first/Same.kt",
                "$ROOT/compiler/util/src/org/jetbrains/kotlin/util/JavaOnly.java",
                "$ROOT/compiler/util/src/org/jetbrains/kotlin/util/JavaUsesKotlin.java",
                "$ROOT/compiler/util/src/org/jetbrains/kotlin/util/Other.kt",
                "$ROOT/compiler/util/src/org/jetbrains/kotlin/util/Util.kt",
                "-classpath",
                util_classpath.as_str(),
                "-Xconsistent-data-class-copy-visibility",
                "-Xskip-prerelease-check",
                "-Xmetadata-version=2.2",
                "-language-version",
                "2.4",
                "-api-version",
                "2.4",
                "-opt-in=krusty.fixture.ExperimentalFirstApi",
                "-opt-in=krusty.fixture.ExperimentalSecondApi",
                "-module-name",
                "kotlin-compiler-util",
                "-jvm-target",
                "17",
                "-jvm-default",
                "no-compatibility",
                "-java-parameters",
                reference_version.as_str(),
                "-jdk-home",
                "$JDK_HOME",
                "-no-stdlib",
                "-no-reflect",
                "-d",
                "$ROOT/compiler/util/build/classes/kotlin/main",
            ])
        );
        assert_eq!(
            normalized_invocation(&runs[friend_at], &root, &kgp),
            strings(&[
                "$ROOT/compiler/util/test-src/org/jetbrains/kotlin/util/Friend.kt",
                "-classpath",
                friend_classpath.as_str(),
                friend_argument.as_str(),
                "-Xconsistent-data-class-copy-visibility",
                "-Xskip-prerelease-check",
                "-Xmetadata-version=2.2",
                "-language-version",
                "2.4",
                "-api-version",
                "2.4",
                "-opt-in=krusty.fixture.ExperimentalFirstApi",
                "-opt-in=krusty.fixture.ExperimentalSecondApi",
                "-module-name",
                "kotlin-compiler-util",
                "-jvm-target",
                "17",
                "-jvm-default",
                "no-compatibility",
                "-java-parameters",
                reference_version.as_str(),
                "-jdk-home",
                "$JDK_HOME",
                "-no-stdlib",
                "-no-reflect",
                "-d",
                "$ROOT/compiler/util/build/classes/kotlin/test",
            ])
        );

        let util = &runs[util_at];
        assert!(util.iter().any(|arg| arg.ends_with("Util.kt")));
        assert!(util.iter().any(|arg| arg.ends_with("Other.kt")));
        assert!(util.iter().any(|arg| arg.ends_with("JavaOnly.java")));
        assert!(util.iter().any(|arg| arg.ends_with("JavaUsesKotlin.java")));
        assert!(util.iter().any(|arg| arg.ends_with("Generated.kt")));
        assert_eq!(
            util.iter().filter(|arg| arg.ends_with("Same.kt")).count(),
            2
        );
        assert!(util.iter().all(|arg| !arg.ends_with("Skip.kts")));
        assert!(util
            .iter()
            .any(|arg| arg == "-Xconsistent-data-class-copy-visibility"));
        assert!(has_pair(util, "-module-name", "kotlin-compiler-util"));
        assert!(has_pair(util, "-jvm-target", "17"));
        assert!(has_pair(util, "-jvm-default", "no-compatibility"));
        assert!(util
            .iter()
            .any(|arg| arg == &format!("-Xkotlin-reference-version={kgp}")));
        assert!(util.iter().any(|arg| arg == "-no-stdlib"));
        assert!(util.iter().any(|arg| arg == "-no-reflect"));
        assert!(util
            .windows(2)
            .any(|pair| pair[0] == "-jdk-home" && !pair[1].is_empty()));
        assert!(util
            .iter()
            .all(|arg| !matches!(arg.as_str(), "-progressive" | "-opt-in")));
        for unique in [
            "-jvm-default",
            "-java-parameters",
            "-jvm-target",
            "-module-name",
            "-jdk-home",
            "-no-stdlib",
            "-no-reflect",
        ] {
            assert_eq!(
                util.iter().filter(|arg| arg.as_str() == unique).count(),
                1,
                "{util:?}"
            );
        }
        let kotlin_classes = root.join("compiler/util/build/classes/kotlin/main");
        assert!(kotlin_classes
            .join("org/jetbrains/kotlin/util/UtilKt.class")
            .is_file());
        let module_metadata = kotlin_classes.join("META-INF/kotlin-compiler-util.kotlin_module");
        assert!(module_metadata.is_file());
        assert!(root
            .join("compiler/util/build/classes/java/main/org/jetbrains/kotlin/util/JavaOnly.class")
            .is_file());
        assert!(root
            .join("compiler/util/build/classes/java/main/org/jetbrains/kotlin/util/JavaUsesKotlin.class")
            .is_file());
        assert!(root
            .join("core/util.runtime/build/classes/java/main/org/jetbrains/kotlin/util/RuntimeJava.class")
            .is_file());
        assert!(root
            .join(
                "compiler/util/build/classes/kotlin/test/org/jetbrains/kotlin/util/FriendKt.class"
            )
            .is_file());

        // Gradle owns task incrementality: an unchanged graph reuses configuration and does not
        // invoke krusty; later probes pin direct source and upstream classpath invalidation.
        let _ = std::fs::remove_file(&log);
        let unchanged =
            run_all_output().unwrap_or_else(|error| panic!("unchanged rebuild: {error}"));
        assert!(!log.exists(), "unchanged sources must not exec krusty");
        assert_eq!(
            unchanged
                .lines()
                .filter(|line| line.trim() == "Reusing configuration cache.")
                .count(),
            1,
            "{unchanged}",
        );

        build()
            .tasks([":core:util.runtime:clean", ":compiler:util:clean"])
            .run()
            .unwrap_or_else(|error| panic!("clean before build-cache restore: {error}"));
        assert!(!root.join("core/util.runtime/build").exists());
        assert!(!root.join("compiler/util/build").exists());
        let _ = std::fs::remove_file(&log);
        let restored = run_all_output()
            .unwrap_or_else(|error| panic!("build-cache restore after clean: {error}"));
        assert!(!log.exists(), "FROM-CACHE tasks must not exec krusty");
        let expected_cache_lines = [
            "> Task :core:util.runtime:compileKotlinWithKrusty FROM-CACHE",
            "> Task :compiler:util:compileKotlinWithKrusty FROM-CACHE",
            "> Task :compiler:util:compileTestKotlinWithKrusty FROM-CACHE",
        ];
        let cache_lines = restored
            .lines()
            .map(str::trim)
            .filter(|line| expected_cache_lines.contains(line))
            .collect::<Vec<_>>();
        assert_eq!(cache_lines, expected_cache_lines, "{restored}");
        assert!(kotlin_classes
            .join("org/jetbrains/kotlin/util/UtilKt.class")
            .is_file());
        assert!(root
            .join(
                "compiler/util/build/classes/kotlin/test/org/jetbrains/kotlin/util/FriendKt.class"
            )
            .is_file());

        let util_source = root.join("compiler/util/src/org/jetbrains/kotlin/util/Util.kt");
        std::fs::write(
            &util_source,
            "package org.jetbrains.kotlin.util\nfun compilerMarker() = runtimeMarker() + RuntimeJava.marker() + JavaOnly.marker()\nfun extra() = 1\n",
        )
        .expect("edit util");
        run_util().unwrap_or_else(|error| panic!("edited util: {error}"));
        let edited = std::fs::read_to_string(&log).expect("edit log");
        let edited_runs = invocations(&edited);
        assert_eq!(edited_runs.len(), 1, "{edited}");
        let edited_sources = source_names(&edited_runs[0]);
        assert_eq!(edited_sources.len(), 7, "{edited_sources:?}");
        for expected in [
            "Generated.kt",
            "JavaOnly.java",
            "JavaUsesKotlin.java",
            "Other.kt",
            "Util.kt",
        ] {
            assert!(edited_sources.contains(&expected), "{edited_sources:?}");
        }
        assert_eq!(
            edited_sources
                .iter()
                .filter(|name| **name == "Same.kt")
                .count(),
            2
        );
        assert!(edited_runs[0].windows(2).any(|pair| {
            pair[0] == "-classpath" && pair[1].contains("util.runtime/build/classes/kotlin/main")
        }));

        let _ = std::fs::remove_file(&log);
        let java_source = root.join("compiler/util/src/org/jetbrains/kotlin/util/JavaOnly.java");
        std::fs::write(
            &java_source,
            "package org.jetbrains.kotlin.util;\npublic class JavaOnly { public static int marker() { return 2; } }\n",
        )
        .expect("edit Java source");
        run_util().unwrap_or_else(|error| panic!("edited Java source: {error}"));
        let java_edited = std::fs::read_to_string(&log).expect("Java edit log");
        let java_edited_runs = invocations(&java_edited);
        assert_eq!(java_edited_runs.len(), 1, "{java_edited}");
        assert_eq!(source_names(&java_edited_runs[0]).len(), 7);
        assert!(source_names(&java_edited_runs[0]).contains(&"JavaOnly.java"));

        // Bring the test compilation current after the earlier public main-source edit. The next
        // probe must measure only the upstream classpath change, not deferred friend invalidation.
        run_all().unwrap_or_else(|error| panic!("upstream baseline: {error}"));
        let _ = std::fs::remove_file(&log);
        let runtime_source =
            root.join("core/util.runtime/src/org/jetbrains/kotlin/util/Runtime.kt");
        std::fs::write(
            &runtime_source,
            "package org.jetbrains.kotlin.util\nfun runtimeMarker() = \"runtime\"\nfun addedRuntimeAbi() = 1\n",
        )
        .expect("edit upstream ABI");
        // The aggregate (the GradleBuild default task) must pull every dirty replacement through
        // the disabled originals, in project-dependency order.
        build()
            .run()
            .unwrap_or_else(|error| panic!("edited upstream ABI: {error}"));
        let upstream_edited = std::fs::read_to_string(&log).expect("upstream edit log");
        let upstream_runs = invocations(&upstream_edited);
        let expected_upstream_order = ["Runtime.kt", "Util.kt", "Friend.kt"];
        assert_eq!(
            upstream_runs.len(),
            expected_upstream_order.len(),
            "{upstream_edited}"
        );
        for (index, source) in expected_upstream_order.iter().enumerate() {
            assert_eq!(
                upstream_runs[index]
                    .iter()
                    .filter(|argument| argument.ends_with(source))
                    .count(),
                1,
                "{upstream_edited}",
            );
            assert_eq!(
                upstream_runs
                    .iter()
                    .filter(|arguments| arguments.iter().any(|argument| argument.ends_with(source)))
                    .count(),
                1,
                "{upstream_edited}",
            );
        }

        let _ = std::fs::remove_file(&log);
        let added = root.join("compiler/util/src/org/jetbrains/kotlin/util/Added.kt");
        std::fs::write(&added, "package org.jetbrains.kotlin.util\nclass Added\n")
            .expect("add source");
        let cleanup_a = root.join("compiler/util/src/org/jetbrains/kotlin/util/CleanupA.kt");
        let cleanup_b = root.join("compiler/util/src/org/jetbrains/kotlin/util/CleanupB.kt");
        std::fs::write(
            &cleanup_a,
            "package org.jetbrains.kotlin.util\nclass CleanupA\n",
        )
        .expect("cleanup survivor");
        std::fs::write(
            &cleanup_b,
            "package org.jetbrains.kotlin.util\nclass CleanupB\n",
        )
        .expect("cleanup removal");
        run_util().unwrap_or_else(|error| panic!("added sources: {error}"));
        assert!(kotlin_classes
            .join("org/jetbrains/kotlin/util/Added.class")
            .is_file());
        assert!(kotlin_classes
            .join("org/jetbrains/kotlin/util/CleanupA.class")
            .is_file());
        assert!(kotlin_classes
            .join("org/jetbrains/kotlin/util/CleanupB.class")
            .is_file());
        assert!(module_metadata.is_file());

        let _ = std::fs::remove_file(&log);
        std::fs::remove_file(&added).expect("remove source");
        std::fs::remove_file(&cleanup_b).expect("remove cleanup source");
        run_util().unwrap_or_else(|error| panic!("removed sources: {error}"));
        assert!(!kotlin_classes
            .join("org/jetbrains/kotlin/util/Added.class")
            .exists());
        assert!(kotlin_classes
            .join("org/jetbrains/kotlin/util/CleanupA.class")
            .is_file());
        assert!(!kotlin_classes
            .join("org/jetbrains/kotlin/util/CleanupB.class")
            .exists());
        assert!(module_metadata.is_file());

        if kgp == "2.4.10" {
            for (case, expected) in [
                (
                    "structured-option",
                    "krusty does not support compilerOptions.progressiveMode",
                ),
                (
                    "reserved-free-argument",
                    "freeCompilerArg '-d=forbidden' conflicts with the plugin-owned destination; configure the structured Gradle input instead",
                ),
                (
                    "duplicate-free-argument",
                    "duplicate freeCompilerArg '-Xlambdas'",
                ),
                (
                    "unknown-free-argument",
                    "unsupported freeCompilerArg '-Xdefinitely-unsupported'; use a supported compilerOptions property",
                ),
                (
                    "plugin-free-argument",
                    "freeCompilerArg '-Xplugin=forbidden.jar' conflicts with compiler plugin configuration; configure the structured Gradle input instead",
                ),
                (
                    "compiler-plugin",
                    "Kotlin compiler plugins are not supported by krusty: org.jetbrains.kotlin.allopen",
                ),
                (
                    "compiler-version",
                    "Kotlin compiler 2.4.0 differs from Kotlin Gradle plugin 2.4.10",
                ),
                (
                    "jvm-default-conflict",
                    "compilerOptions.jvmDefault and freeCompilerArg '-jvm-default=disable' are both set; configure exactly one",
                ),
                (
                    "jvm-default-bad-mode",
                    "unsupported freeCompilerArg '-jvm-default=sideways'; use a supported compilerOptions property",
                ),
                (
                    "bare-metadata-version",
                    "freeCompilerArg '-Xmetadata-version' needs the '=' form: -Xmetadata-version=<major.minor>",
                ),
                (
                    "bad-metadata-version",
                    "unsupported freeCompilerArg '-Xmetadata-version=2.5'; supported metadata versions: 2.0, 2.1, 2.2, 2.3, 2.4",
                ),
                (
                    "empty-metadata-version",
                    "unsupported freeCompilerArg '-Xmetadata-version='; supported metadata versions: 2.0, 2.1, 2.2, 2.3, 2.4",
                ),
                (
                    "old-language-version",
                    "krusty does not support compilerOptions.languageVersion=2.2; only 2.4 is supported",
                ),
                (
                    "old-api-version",
                    "krusty does not support compilerOptions.apiVersion=2.0; only 2.4 is supported",
                ),
                (
                    "progressive-free-argument",
                    "freeCompilerArg '-progressive' conflicts with compilerOptions.progressiveMode; configure the structured Gradle input instead",
                ),
                (
                    "jspecify-free-argument",
                    "unsupported freeCompilerArg '-Xjspecify-annotations=strict'; use a supported compilerOptions property",
                ),
                (
                    "jdk-release-free-argument",
                    "unsupported freeCompilerArg '-Xjdk-release=8'; use a supported compilerOptions property",
                ),
                (
                    "duplicate-inert-flag",
                    "duplicate freeCompilerArg '-Xskip-prerelease-check'",
                ),
                (
                    "all-warnings-as-errors",
                    "krusty does not support compilerOptions.allWarningsAsErrors",
                ),
                (
                    "free-werror",
                    "krusty does not support warning policy freeCompilerArg '-Werror'",
                ),
                (
                    "warning-level",
                    "krusty does not support warning policy freeCompilerArg '-Xwarning-level=REDUNDANT_CLI_ARG:disabled'",
                ),
                (
                    "empty-opt-in",
                    "compilerOptions.optIn contains an empty marker",
                ),
                (
                    "duplicate-opt-in",
                    "compilerOptions.optIn contains duplicate marker 'krusty.fixture.ExperimentalFirstApi'",
                ),
                (
                    "opt-in-overlap",
                    "compilerOptions.optIn and freeCompilerArg '-opt-in=krusty.fixture.ExperimentalFirstApi' both request marker 'krusty.fixture.ExperimentalFirstApi'; configure exactly one",
                ),
                (
                    "jvm-target",
                    "Kotlin JVM target 17 differs from Java target 11",
                ),
            ] {
                let _ = std::fs::remove_file(&log);
                let result = build()
                    .property("krusty.negative", case)
                    .tasks([":compiler:util:compileKotlin"])
                    .run();
                let error = match result {
                    Ok(()) => panic!("negative case {case} succeeded"),
                    Err(error) => error,
                };
                let rendered = error.to_string();
                let expected_line = format!("> {expected}");
                assert_eq!(
                    rendered
                        .lines()
                        .filter(|line| line.trim() == expected_line)
                        .count(),
                    1,
                    "{case}: {rendered}",
                );
                assert!(!log.exists(), "{case} must fail before execing krusty");
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    fn quoted_release_versions(text: &str) -> Vec<&str> {
        text.split('"')
            .skip(1)
            .step_by(2)
            .filter(|value| {
                value.split('.').count() == 3
                    && value.split('.').all(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
            })
            .collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn normalized_path_list(values: &[&str]) -> String {
        std::env::join_paths(values)
            .expect("normalized path list")
            .to_string_lossy()
            .into_owned()
    }

    fn normalize_path(path: &Path, root: &Path, kotlin_version: &str) -> String {
        assert!(
            path.is_absolute(),
            "non-absolute input path: {}",
            path.display()
        );
        if let Ok(relative) = path.strip_prefix(root) {
            return format!("$ROOT/{}", relative.display());
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("non-UTF-8 dependency path: {}", path.display()));
        if file_name == format!("kotlin-stdlib-{kotlin_version}.jar") {
            "$KOTLIN_STDLIB".to_owned()
        } else if file_name == "annotations-13.0.jar" {
            "$ANNOTATIONS".to_owned()
        } else {
            panic!("unexpected dependency path: {}", path.display())
        }
    }

    fn normalize_paths(paths: &str, root: &Path, kotlin_version: &str) -> String {
        let normalized = std::env::split_paths(paths)
            .map(|path| normalize_path(&path, root, kotlin_version))
            .collect::<Vec<_>>();
        assert!(!normalized.is_empty(), "empty path list");
        std::env::join_paths(normalized)
            .expect("normalized path list")
            .to_string_lossy()
            .into_owned()
    }

    fn normalized_invocation(
        arguments: &[String],
        root: &Path,
        kotlin_version: &str,
    ) -> Vec<String> {
        let root_text = root.to_string_lossy();
        let mut normalized = Vec::with_capacity(arguments.len());
        let mut index = 0;
        while index < arguments.len() {
            let argument = &arguments[index];
            if argument == "-classpath" {
                normalized.push(argument.clone());
                let classpath = arguments.get(index + 1).expect("classpath value");
                normalized.push(normalize_paths(classpath, root, kotlin_version));
                index += 2;
                continue;
            }
            if argument.starts_with("-Xfriend-paths=") {
                let paths = argument.trim_start_matches("-Xfriend-paths=");
                normalized.push(format!(
                    "-Xfriend-paths={}",
                    normalize_paths(paths, root, kotlin_version)
                ));
                index += 1;
                continue;
            }
            if argument == "-jdk-home" {
                normalized.push(argument.clone());
                let jdk = arguments.get(index + 1).expect("JDK home value");
                assert!(Path::new(jdk).is_absolute(), "non-absolute JDK home: {jdk}");
                normalized.push("$JDK_HOME".to_owned());
                index += 2;
                continue;
            }
            normalized.push(argument.replace(root_text.as_ref(), "$ROOT"));
            index += 1;
        }
        normalized
    }

    fn source_names(args: &[String]) -> Vec<&str> {
        args.iter()
            .filter_map(|arg| arg.rsplit('/').next())
            .filter(|name| name.ends_with(".kt") || name.ends_with(".java"))
            .collect()
    }

    fn has_pair(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    fn invocations(text: &str) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        let mut current = Vec::new();
        for line in text.lines() {
            if line == "----" {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                continue;
            }
            current.push(line.to_string());
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }

    fn write_krusty_proxy(path: &Path, log: &Path, actual: &Path) {
        let quote = |value: &Path| value.display().to_string().replace('\'', "'\\''");
        let script = r#"#!/bin/sh
{
  echo "----"
  printf '%s\n' "$@"
} >> "LOGPATH"
exec 'ACTUAL' "$@"
"#
        .replace("LOGPATH", &quote(log))
        .replace("ACTUAL", &quote(actual));
        std::fs::write(path, script).expect("write recording krusty");
    }

    fn ensure_gradle() -> PathBuf {
        if let Some(bin) = std::env::var_os("KRUSTY_GRADLE_BIN").map(PathBuf::from) {
            assert!(
                bin.is_file(),
                "KRUSTY_GRADLE_BIN not found at {}",
                bin.display()
            );
            return bin;
        }
        let version =
            std::env::var("KRUSTY_GRADLE_VERSION").unwrap_or_else(|_| "8.14.3".to_owned());
        let cache = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cache");
        let bin = cache.join(format!("gradle-{version}/bin/gradle"));
        if bin.is_file() {
            return bin;
        }
        std::fs::create_dir_all(&cache).expect("gradle cache");
        let zip = cache.join(format!("gradle-{version}-bin.zip"));
        let url = format!("https://services.gradle.org/distributions/gradle-{version}-bin.zip");
        let checksum = cache.join(format!("gradle-{version}-bin.zip.sha256"));
        let checksum_url = format!("{url}.sha256");
        let status = Command::new("curl")
            .args([
                "-fsSL",
                "--retry",
                "5",
                "--retry-all-errors",
                "--connect-timeout",
                "20",
                "-o",
                &zip.to_string_lossy(),
                &url,
            ])
            .status()
            .expect("curl");
        assert!(status.success(), "download gradle");
        let status = Command::new("curl")
            .args([
                "-fsSL",
                "--retry",
                "5",
                "--retry-all-errors",
                "--connect-timeout",
                "20",
                "-o",
                &checksum.to_string_lossy(),
                &checksum_url,
            ])
            .status()
            .expect("curl checksum");
        assert!(status.success(), "download Gradle checksum");
        let expected = std::fs::read_to_string(&checksum)
            .expect("read Gradle checksum")
            .trim()
            .to_owned();
        let output = Command::new("sha256sum")
            .arg(&zip)
            .output()
            .expect("sha256sum Gradle distribution");
        assert!(output.status.success(), "hash Gradle distribution");
        let actual = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .expect("Gradle distribution hash")
            .to_owned();
        assert_eq!(actual, expected, "Gradle distribution checksum");
        let status = Command::new("unzip")
            .args([
                "-q",
                "-o",
                &zip.to_string_lossy(),
                "-d",
                &cache.to_string_lossy(),
            ])
            .status()
            .expect("unzip");
        assert!(status.success() && bin.is_file(), "unpack gradle");
        bin
    }

    fn write_compiler_slice(root: &Path, plugin_version: &str, kgp_version: &str) {
        let write = |rel: &str, body: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            let body = body
                .replace("PLUGIN_VERSION", plugin_version)
                .replace("KGP_VERSION", kgp_version);
            std::fs::write(path, body).expect("write");
        };
        write(
            "settings.gradle.kts",
            r#"
pluginManagement {
    repositories {
        providers.gradleProperty("krusty.plugin.repository").orNull?.let {
            maven { url = uri(it) }
        }
        gradlePluginPortal()
        mavenCentral()
    }
}

rootProject.name = "kotlin-compiler-slice"
include(":core:util.runtime")
include(":compiler:util")
"#,
        );
        write(
            "build.gradle.kts",
            r#"
subprojects {
    repositories {
        mavenCentral()
    }
}
"#,
        );
        write(
            "core/util.runtime/build.gradle.kts",
            r#"
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import org.gradle.api.tasks.compile.JavaCompile

plugins {
    kotlin("jvm") version "KGP_VERSION"
    id("krusty") version "PLUGIN_VERSION"
    `java-library`
}

val krustyNegative = providers.gradleProperty("krusty.negative").orNull

kotlin {
    sourceSets.named("main") {
        kotlin.srcDir("src")
    }
}

sourceSets.named("main") {
    java.setSrcDirs(listOf("src"))
}

tasks.withType<KotlinJvmCompile>().configureEach {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        moduleName.set("kotlin-util-runtime")
        languageVersion.set(KotlinVersion.KOTLIN_2_4)
        apiVersion.set(KotlinVersion.KOTLIN_2_4)
        jvmDefault.set(JvmDefaultMode.NO_COMPATIBILITY)
        javaParameters.set(true)
        freeCompilerArgs.add("-Xconsistent-data-class-copy-visibility")
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(if (krustyNegative == "jvm-target") 11 else 17)
}
"#,
        );
        write(
            "core/util.runtime/src/org/jetbrains/kotlin/util/Runtime.kt",
            "package org.jetbrains.kotlin.util\nfun runtimeMarker() = \"runtime\"\n",
        );
        write(
            "core/util.runtime/src/org/jetbrains/kotlin/util/RuntimeJava.java",
            "package org.jetbrains.kotlin.util;\npublic class RuntimeJava { public static String marker() { return \"java\"; } }\n",
        );
        write(
            "compiler/util/build.gradle.kts",
            r#"
@file:OptIn(
    org.jetbrains.kotlin.buildtools.api.ExperimentalBuildToolsApi::class,
    org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi::class,
)

import org.jetbrains.kotlin.gradle.dsl.JvmDefaultMode
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile
import org.gradle.api.DefaultTask
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.TaskAction
import org.gradle.api.tasks.compile.JavaCompile

plugins {
    kotlin("jvm") version "KGP_VERSION"
    id("krusty") version "PLUGIN_VERSION"
    kotlin("plugin.allopen") version "KGP_VERSION" apply false
    `java-library`
}

dependencies {
    api(project(":core:util.runtime"))
}

val krustyNegative = providers.gradleProperty("krusty.negative").orNull

if (krustyNegative == "compiler-plugin") {
    pluginManager.apply("org.jetbrains.kotlin.plugin.allopen")
}

abstract class GenerateKotlin : DefaultTask() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @TaskAction
    fun generate() {
        outputDirectory.get().file("org/jetbrains/kotlin/util/Generated.kt").asFile.apply {
            parentFile.mkdirs()
            writeText("package org.jetbrains.kotlin.util\nclass Generated\n")
        }
    }
}

val generateKotlin = tasks.register<GenerateKotlin>("generateKotlin") {
    outputDirectory.set(layout.buildDirectory.dir("generated/krusty"))
}

kotlin {
    if (krustyNegative == "compiler-version") {
        compilerVersion.set("2.4.0")
    }
    sourceSets.named("main") {
        kotlin.srcDir("src")
        kotlin.srcDir("other-src")
        kotlin.srcDir(generateKotlin.flatMap { it.outputDirectory })
    }
    sourceSets.named("test") {
        kotlin.srcDir("test-src")
    }
}

sourceSets.named("main") {
    java.setSrcDirs(listOf("src"))
    resources.srcDir("resources")
}

tasks.withType<KotlinJvmCompile>().configureEach {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        moduleName.set("kotlin-compiler-util")
        languageVersion.set(KotlinVersion.KOTLIN_2_4)
        apiVersion.set(KotlinVersion.KOTLIN_2_4)
        jvmDefault.set(JvmDefaultMode.NO_COMPATIBILITY)
        javaParameters.set(true)
        optIn.set(
            when (krustyNegative) {
                "empty-opt-in" -> listOf("krusty.fixture.ExperimentalFirstApi", "")
                "duplicate-opt-in" -> listOf(
                    "krusty.fixture.ExperimentalFirstApi",
                    "krusty.fixture.ExperimentalFirstApi",
                )
                else -> listOf(
                    "krusty.fixture.ExperimentalFirstApi",
                    "krusty.fixture.ExperimentalSecondApi",
                )
            },
        )
        freeCompilerArgs.add("-Xconsistent-data-class-copy-visibility")
        freeCompilerArgs.add("-Xskip-prerelease-check")
        freeCompilerArgs.add("-Xmetadata-version=2.2")
        when (krustyNegative) {
            "structured-option" -> progressiveMode.set(true)
            "all-warnings-as-errors" -> allWarningsAsErrors.set(true)
            "reserved-free-argument" -> freeCompilerArgs.add("-d=forbidden")
            "duplicate-free-argument" -> freeCompilerArgs.addAll(
                listOf("-Xlambdas=indy", "-Xlambdas=class"),
            )
            "unknown-free-argument" -> freeCompilerArgs.add("-Xdefinitely-unsupported")
            "plugin-free-argument" -> freeCompilerArgs.add("-Xplugin=forbidden.jar")
            "jvm-default-conflict" -> freeCompilerArgs.add("-jvm-default=disable")
            "jvm-default-bad-mode" -> freeCompilerArgs.add("-jvm-default=sideways")
            "bare-metadata-version" -> freeCompilerArgs.add("-Xmetadata-version")
            "bad-metadata-version" -> freeCompilerArgs.add("-Xmetadata-version=2.5")
            "empty-metadata-version" -> freeCompilerArgs.add("-Xmetadata-version=")
            "old-language-version" -> languageVersion.set(KotlinVersion.KOTLIN_2_2)
            "old-api-version" -> apiVersion.set(KotlinVersion.KOTLIN_2_0)
            "progressive-free-argument" -> freeCompilerArgs.add("-progressive")
            "jspecify-free-argument" -> freeCompilerArgs.add("-Xjspecify-annotations=strict")
            "jdk-release-free-argument" -> freeCompilerArgs.add("-Xjdk-release=8")
            "duplicate-inert-flag" -> freeCompilerArgs.add("-Xskip-prerelease-check")
            "free-werror" -> freeCompilerArgs.add("-Werror")
            "warning-level" -> freeCompilerArgs.add("-Xwarning-level=REDUNDANT_CLI_ARG:disabled")
            "opt-in-overlap" -> freeCompilerArgs.add("-opt-in=krusty.fixture.ExperimentalFirstApi")
        }
    }
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(17)
}
"#,
        );
        write(
            "compiler/util/src/org/jetbrains/kotlin/util/Util.kt",
            "package org.jetbrains.kotlin.util\nfun compilerMarker() = runtimeMarker() + RuntimeJava.marker() + JavaOnly.marker()\n",
        );
        write(
            "compiler/util/src/org/jetbrains/kotlin/util/Other.kt",
            "package org.jetbrains.kotlin.util\ninternal fun otherMarker() = 1\n",
        );
        write(
            "compiler/util/src/first/Same.kt",
            "package first\nclass Same\n",
        );
        write(
            "compiler/util/other-src/second/Same.kt",
            "package second\nclass Same\n",
        );
        write(
            "compiler/util/test-src/org/jetbrains/kotlin/util/Friend.kt",
            "package org.jetbrains.kotlin.util\nfun friendMarker() = otherMarker()\n",
        );
        write(
            "compiler/util/src/org/jetbrains/kotlin/util/Skip.kts",
            "println(\"script\")\n",
        );
        write(
            "compiler/util/src/org/jetbrains/kotlin/util/JavaOnly.java",
            "package org.jetbrains.kotlin.util;\npublic class JavaOnly { public static int marker() { return 1; } }\n",
        );
        write(
            "compiler/util/src/org/jetbrains/kotlin/util/JavaUsesKotlin.java",
            "package org.jetbrains.kotlin.util;\npublic class JavaUsesKotlin { public static String marker() { return UtilKt.compilerMarker(); } }\n",
        );
        write("compiler/util/resources/notice.txt", "notice\n");
    }
}
