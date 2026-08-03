//! Process options specific to the language-server executable.

use std::path::{Path, PathBuf};

use krusty::features::LangFeatures;
use krusty::jvm::classpath::platform_jdk_modules;

#[derive(Default)]
pub struct LspOptions {
    classpath: Vec<PathBuf>,
    jdk_home: Option<PathBuf>,
    no_jdk: bool,
    language_arguments: Vec<String>,
    deps_cache_dir: Option<PathBuf>,
    deps_cache_max_age_days: Option<u64>,
    deps_cache_max_bytes: Option<u64>,
    deps_sources: Option<bool>,
    dev: bool,
}

impl LspOptions {
    pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = argv.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--stdio" => {}
                "--dev" => options.dev = true,
                "-cp" | "-classpath" | "-class-path" => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{argument} requires a value"))?;
                    options.classpath.extend(std::env::split_paths(&value));
                }
                "-jdk-home" => {
                    options.jdk_home = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "-jdk-home requires a value".to_string())?,
                    ));
                }
                "-no-jdk" => options.no_jdk = true,
                "-deps-cache-dir" => {
                    options.deps_cache_dir =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            "-deps-cache-dir requires a value".to_string()
                        })?));
                }
                "-deps-cache-max-age-days" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "-deps-cache-max-age-days requires a value".to_string())?;
                    options.deps_cache_max_age_days = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid -deps-cache-max-age-days '{value}'"))?,
                    );
                }
                "-deps-cache-max-bytes" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "-deps-cache-max-bytes requires a value".to_string())?;
                    options.deps_cache_max_bytes = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid -deps-cache-max-bytes '{value}'"))?,
                    );
                }
                "-deps-sources" => options.deps_sources = Some(true),
                "-no-deps-sources" => options.deps_sources = Some(false),
                _ => {
                    if LangFeatures::new().apply_cli_arg(&argument) {
                        options.language_arguments.push(argument);
                    } else {
                        return Err(format!("unsupported option '{argument}'"));
                    }
                }
            }
        }
        Ok(options)
    }

    pub fn effective_classpath(&self) -> Vec<PathBuf> {
        effective_classpath_for(&self.classpath, self.jdk_home.as_deref(), self.no_jdk)
    }

    /// The classpath passed explicitly with `-cp`, without JDK modules. Empty means "no explicit
    /// classpath" — the trigger for the server to resolve one from the build tool.
    pub fn explicit_classpath(&self) -> &[PathBuf] {
        &self.classpath
    }

    /// The `-jdk-home` hint, if one was given.
    pub fn jdk_home(&self) -> Option<&Path> {
        self.jdk_home.as_deref()
    }

    /// Whether `-no-jdk` was passed: the server must not attach a JDK.
    pub fn no_jdk(&self) -> bool {
        self.no_jdk
    }

    pub fn language_features(&self) -> LangFeatures {
        let mut features = LangFeatures::new();
        self.apply_language_features(&mut features);
        features
    }

    /// Apply explicit LSP flags over project-derived features, preserving CLI order and disables.
    pub fn apply_language_features(&self, features: &mut LangFeatures) {
        for argument in &self.language_arguments {
            features.apply_cli_arg(argument);
        }
    }

    pub fn language_arguments(&self) -> &[String] {
        &self.language_arguments
    }

    pub fn deps_cache_dir(&self) -> Option<&Path> {
        self.deps_cache_dir.as_deref()
    }

    pub fn deps_cache_max_age_days(&self) -> u64 {
        self.deps_cache_max_age_days.unwrap_or(30)
    }

    pub fn deps_cache_max_bytes(&self) -> u64 {
        self.deps_cache_max_bytes.unwrap_or(512 * 1024 * 1024)
    }

    pub fn deps_sources_enabled(&self) -> bool {
        self.deps_sources.unwrap_or(true)
    }

    /// Dev mode: enables the AST/checker/IR dump code action. Off by default so a normal editor
    /// session advertises no extra capabilities and retains no extra state.
    pub fn dev(&self) -> bool {
        self.dev
    }
}

/// Compose the classpath used by an analysis process from project/CLI entries and the selected JDK.
/// Initial startup and project-model reconfiguration share this function so they cannot disagree
/// about `-no-jdk`, JDK discovery, ordering, or duplicate a worker-only version of option semantics.
pub(crate) fn effective_classpath_for(
    classpath: &[PathBuf],
    jdk_home: Option<&Path>,
    no_jdk: bool,
) -> Vec<PathBuf> {
    let mut effective = classpath.to_vec();
    effective.extend(effective_platform_classpath(jdk_home, no_jdk));
    effective
}

/// The platform-only portion of an analysis classpath. Project grouping needs this separately when
/// it builds per-module classpaths, but it must use the same JDK/no-JDK rule as the worker's complete
/// launch classpath rather than maintaining another conditional in the server binary.
pub fn effective_platform_classpath(jdk_home: Option<&Path>, no_jdk: bool) -> Vec<PathBuf> {
    if no_jdk {
        Vec::new()
    } else {
        platform_jdk_modules(jdk_home).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LspOptions, String> {
        LspOptions::parse(args.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn parses_dependency_cache_options_with_defaults() {
        let options = parse(&[
            "--stdio",
            "-deps-cache-dir",
            "/tmp/dc",
            "-deps-cache-max-age-days",
            "7",
            "-no-deps-sources",
        ])
        .unwrap();
        assert_eq!(options.deps_cache_dir(), Some(Path::new("/tmp/dc")));
        assert_eq!(options.deps_cache_max_age_days(), 7);
        assert!(!options.deps_sources_enabled());
        assert_eq!(options.deps_cache_max_bytes(), 512 * 1024 * 1024);

        let defaults = parse(&["--stdio"]).unwrap();
        assert_eq!(defaults.deps_cache_dir(), None);
        assert_eq!(defaults.deps_cache_max_age_days(), 30);
        assert!(defaults.deps_sources_enabled());
    }

    #[test]
    fn accepts_language_server_and_language_feature_options() {
        let options = parse(&[
            "--stdio",
            "-cp",
            "a.jar:b/classes",
            "-no-jdk",
            "-Xname-based-destructuring=complete",
        ])
        .unwrap();
        assert_eq!(
            options.effective_classpath(),
            vec![PathBuf::from("a.jar"), PathBuf::from("b/classes")]
        );
        assert!(
            effective_platform_classpath(Some(Path::new("/ignored-jdk")), true).is_empty(),
            "-no-jdk must suppress platform modules for every classpath consumer"
        );
        let features = options.language_features();
        assert!(features.has("NameBasedDestructuring"));
        let options = parse(&[
            "-Xname-based-destructuring=complete",
            "-Xname-based-destructuring=disable",
        ])
        .unwrap();
        let features = options.language_features();
        assert!(!features.has("NameBasedDestructuring"));
        let mut project_features = LangFeatures::new();
        project_features.enable("NameBasedDestructuring");
        project_features.enable("EnableNameBasedDestructuringShortForm");
        options.apply_language_features(&mut project_features);
        assert!(!project_features.has("NameBasedDestructuring"));
        assert!(!project_features.has("EnableNameBasedDestructuringShortForm"));
        assert!(parse(&["Main.kt"]).is_err());
        assert!(parse(&["-d", "out"]).is_err());
    }

    #[test]
    fn missing_option_values_are_errors() {
        assert_eq!(
            parse(&["-cp"]).err().as_deref(),
            Some("-cp requires a value")
        );
        assert_eq!(
            parse(&["-jdk-home"]).err().as_deref(),
            Some("-jdk-home requires a value")
        );
    }

    #[test]
    fn dev_mode_is_off_unless_requested() {
        let options = LspOptions::parse(["--stdio".to_string()]).unwrap();
        assert!(!options.dev());
    }

    #[test]
    fn dev_flag_turns_dev_mode_on() {
        let options = LspOptions::parse(["--stdio".to_string(), "--dev".to_string()]).unwrap();
        assert!(options.dev());
    }
}
