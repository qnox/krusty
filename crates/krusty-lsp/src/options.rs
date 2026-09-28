//! Process options specific to the language-server executable.

use std::path::{Path, PathBuf};

use krusty::features::LangFeatures;
use krusty::jvm::classpath::platform_jdk_modules;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warning,
    Error,
    Off,
    All,
}

impl LogLevel {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "TRACE" => Ok(Self::Trace),
            "DEBUG" => Ok(Self::Debug),
            "INFO" => Ok(Self::Info),
            "WARNING" => Ok(Self::Warning),
            "ERROR" => Ok(Self::Error),
            "OFF" => Ok(Self::Off),
            "ALL" => Ok(Self::All),
            _ => Err(format!(
                "'{value}' is not a valid log level. Supported values: TRACE, DEBUG, INFO, \
                 WARNING, ERROR, OFF, ALL (case-sensitive)"
            )),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum OptionsError {
    Help(String),
    Invalid(String),
}

#[derive(Debug, Default)]
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
    log_level: LogLevel,
    log_categories: Vec<(String, LogLevel)>,
}

impl LspOptions {
    pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Self, OptionsError> {
        Self::parse_with(argv, |key| std::env::var(key).ok())
    }

    /// Parse `argv`, printing `--help` or writing a startup error and exiting. The supervisor uses
    /// this so a help request ends with status 0 and every other rejection ends with status 2.
    pub fn parse_or_exit(argv: impl IntoIterator<Item = String>) -> Self {
        match Self::parse(argv) {
            Ok(options) => options,
            Err(OptionsError::Help(text)) => {
                println!("{text}");
                std::process::exit(0);
            }
            Err(OptionsError::Invalid(error)) => {
                eprintln!("krusty-lsp: {error}");
                std::process::exit(2);
            }
        }
    }

    fn parse_with(
        argv: impl IntoIterator<Item = String>,
        mut env: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, OptionsError> {
        let arguments: Vec<String> = argv.into_iter().collect();
        if arguments
            .iter()
            .any(|argument| argument == "--help" || argument == "-h")
        {
            return Err(OptionsError::Help(help_text()));
        }
        Self::parse_arguments(&arguments, &mut env).map_err(OptionsError::Invalid)
    }

    fn parse_arguments(
        arguments: &[String],
        env: &mut impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = arguments.iter().cloned();
        let mut stdio = false;
        let mut client = false;
        let mut multi_client = false;
        let mut socket = false;
        let mut log_level_from_args = false;
        let mut log_categories_from_args = false;
        while let Some(argument) = args.next() {
            if let Some(value) = option_value(&argument, "--socket", &mut args)? {
                parse_socket(&value)?;
                socket = true;
                continue;
            }
            if let Some(value) = option_value(&argument, "--system-path", &mut args)? {
                set_cache_dir(&mut options.deps_cache_dir, value)?;
                continue;
            }
            if let Some(value) = option_value(&argument, "--log-level", &mut args)? {
                options.log_level = LogLevel::parse(&value)?;
                log_level_from_args = true;
                continue;
            }
            if let Some(value) = option_value(&argument, "--log-category", &mut args)? {
                push_log_categories(&mut options.log_categories, &value)?;
                log_categories_from_args = true;
                continue;
            }
            match argument.as_str() {
                "--stdio" => stdio = true,
                "--client" => client = true,
                "--multi-client" => multi_client = true,
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
                    let value = args
                        .next()
                        .ok_or_else(|| "-deps-cache-dir requires a value".to_string())?;
                    set_cache_dir(&mut options.deps_cache_dir, value)?;
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
        if client && stdio {
            return Err("Can't use stdio mode with client mode".to_string());
        }
        if multi_client && stdio {
            return Err("Stdio mode doesn't support multiclient mode".to_string());
        }
        if multi_client && client {
            return Err("Client mode doesn't support multiclient mode".to_string());
        }
        // No transport flag keeps the historical stdio default. An explicit socket selection must
        // not start that default and leave the client talking to a port nothing listens on.
        if !stdio && (client || multi_client || socket) {
            return Err("socket transport is not supported; pass --stdio".to_string());
        }
        if !log_level_from_args {
            if let Some(value) = env("KOTLIN_LSP_LOG_LEVEL") {
                options.log_level = LogLevel::parse(&value)?;
            }
        }
        if !log_categories_from_args {
            if let Some(value) = env("KOTLIN_LSP_LOG_CATEGORIES") {
                push_log_categories(&mut options.log_categories, &value)?;
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

    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }

    pub fn log_categories(&self) -> &[(String, LogLevel)] {
        &self.log_categories
    }
}

fn option_value(
    argument: &str,
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<Option<String>, String> {
    let Some(rest) = argument.strip_prefix(name) else {
        return Ok(None);
    };
    if let Some(value) = rest.strip_prefix('=') {
        if value.is_empty() {
            return Err(format!("{name} requires a value"));
        }
        return Ok(Some(value.to_string()));
    }
    if rest.is_empty() {
        return args
            .next()
            .map(Some)
            .ok_or_else(|| format!("{name} requires a value"));
    }
    Ok(None)
}

fn set_cache_dir(slot: &mut Option<PathBuf>, value: String) -> Result<(), String> {
    if slot.is_some() {
        return Err("pass either --system-path or -deps-cache-dir".to_string());
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn parse_socket(value: &str) -> Result<(), String> {
    let port = value.split_once(':').map_or(value, |(_, port)| port);
    if port.parse::<i32>().is_err() {
        return Err(format!(
            "'{value}' is not a valid socket. Expected <host>:<port>"
        ));
    }
    Ok(())
}

fn push_log_categories(
    categories: &mut Vec<(String, LogLevel)>,
    value: &str,
) -> Result<(), String> {
    for entry in value.split(',') {
        let entry = entry.trim();
        let Some((category, level)) = entry.split_once(':') else {
            return Err(category_level_error(entry));
        };
        if category.is_empty() || level.is_empty() {
            return Err(category_level_error(entry));
        }
        let category = category.trim();
        if category.is_empty() {
            return Err(format!("Category must be non-empty in '{entry}'"));
        }
        categories.push((category.to_string(), LogLevel::parse(level.trim())?));
    }
    Ok(())
}

fn category_level_error(value: &str) -> String {
    format!(
        "'{value}' is not a valid category:level. Expected format <category>:<level>, \
         e.g. com.example:DEBUG"
    )
}

fn help_text() -> String {
    "\
Usage: krusty-lsp [options]

  --stdio
      Read and write Language Server Protocol messages on standard input and output.
  --socket <host:port>
      Accepted together with --stdio. Socket transport is not supported on its own.
  --client
      Connect out to a socket. Not supported, and it conflicts with --stdio.
  --multi-client
      Accept another client after disconnect. Not supported, and it conflicts with --stdio and --client.
  --system-path <path>
      Directory for caches and indexes. The same directory as -deps-cache-dir.
  --log-level <LEVEL>
      TRACE, DEBUG, INFO, WARNING, ERROR, OFF, or ALL. Case-sensitive.
      KOTLIN_LSP_LOG_LEVEL supplies the value when the option is absent.
  --log-category <category:LEVEL>
      Per-category level. Repeat the option, or separate entries with commas.
      KOTLIN_LSP_LOG_CATEGORIES supplies the value when the option is absent.
  -cp, -classpath, -class-path <path>
  -jdk-home <path>
  -no-jdk
  -deps-cache-dir <path>
  -deps-cache-max-age-days <days>
  -deps-cache-max-bytes <bytes>
  -deps-sources, -no-deps-sources
  --dev
  -h, --help
      Compiler -X language flags are accepted as well.
"
    .to_string()
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
        parse_env(args, |_| None)
    }

    fn parse_env(
        args: &[&str],
        env: impl FnMut(&str) -> Option<String>,
    ) -> Result<LspOptions, String> {
        LspOptions::parse_with(args.iter().map(|argument| argument.to_string()), env).map_err(
            |error| match error {
                OptionsError::Invalid(text) | OptionsError::Help(text) => text,
            },
        )
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

    #[test]
    fn stdio_launch_accepts_kotlin_lsp_cache_and_log_options() {
        let options = parse(&[
            "--stdio",
            "--system-path=/tmp/kotlin-lsp",
            "--log-level=DEBUG",
            "--log-category=com.example:TRACE,indexing:INFO",
            "--socket=127.0.0.1:9999",
        ])
        .unwrap();
        assert_eq!(options.deps_cache_dir(), Some(Path::new("/tmp/kotlin-lsp")));
        assert_eq!(options.log_level(), LogLevel::Debug);
        assert_eq!(
            options.log_categories(),
            &[
                ("com.example".to_string(), LogLevel::Trace),
                ("indexing".to_string(), LogLevel::Info),
            ]
        );
    }

    #[test]
    fn log_options_fall_back_to_the_kotlin_lsp_environment() {
        let options = parse_env(&["--stdio"], |key| match key {
            "KOTLIN_LSP_LOG_LEVEL" => Some("WARNING".to_string()),
            "KOTLIN_LSP_LOG_CATEGORIES" => Some("resolve:ERROR".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(options.log_level(), LogLevel::Warning);
        assert_eq!(
            options.log_categories(),
            &[("resolve".to_string(), LogLevel::Error)]
        );

        let overridden = parse_env(
            &[
                "--stdio",
                "--log-level",
                "OFF",
                "--log-category",
                "editor:DEBUG",
            ],
            |_| Some("TRACE".to_string()),
        )
        .unwrap();
        assert_eq!(overridden.log_level(), LogLevel::Off);
        assert_eq!(
            overridden.log_categories(),
            &[("editor".to_string(), LogLevel::Debug)]
        );
    }

    #[test]
    fn kotlin_lsp_launch_rejects_a_bad_level_category_or_socket() {
        assert!(parse(&["--stdio", "--log-level=debug"])
            .unwrap_err()
            .contains("case-sensitive"));
        assert!(parse(&["--stdio", "--log-category=:DEBUG"])
            .unwrap_err()
            .contains("not a valid category:level"));
        assert!(parse(&["--stdio", "--log-category= :DEBUG"])
            .unwrap_err()
            .contains("not a valid category:level"));
        assert_eq!(
            parse(&["--stdio", "--socket", "nope"]).unwrap_err(),
            "'nope' is not a valid socket. Expected <host>:<port>"
        );
        assert_eq!(
            parse(&["--socket=9999"]).unwrap_err(),
            "socket transport is not supported; pass --stdio"
        );
        assert_eq!(
            parse(&["--stdio", "--client"]).unwrap_err(),
            "Can't use stdio mode with client mode"
        );
        assert_eq!(
            parse(&["--stdio", "--multi-client"]).unwrap_err(),
            "Stdio mode doesn't support multiclient mode"
        );
        assert_eq!(
            parse(&["--client", "--multi-client"]).unwrap_err(),
            "Client mode doesn't support multiclient mode"
        );
        assert_eq!(
            parse(&["--system-path", "/a", "-deps-cache-dir", "/b"]).unwrap_err(),
            "pass either --system-path or -deps-cache-dir"
        );
    }

    #[test]
    fn help_is_not_a_startup_failure() {
        let help = parse(&["--help"]).unwrap_err();
        assert!(help.contains("--stdio"));
        assert!(help.contains("--system-path"));
        assert!(parse(&["-h"]).unwrap_err().contains("--log-level"));
    }
}
