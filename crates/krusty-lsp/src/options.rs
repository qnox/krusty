//! Process options specific to the language-server executable.

use std::path::{Path, PathBuf};

use krusty::jvm::classpath::platform_jdk_modules;
use krusty::language_settings::LanguageSettings;
use krusty_cli::cli::LanguageArgument;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LogLevel {
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
                print!("{text}");
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
        let mut log_level_arg = None;
        let mut log_category_args = Vec::new();
        while let Some(argument) = args.next() {
            if let Some(value) = option_value(&argument, "--socket", &mut args)? {
                parse_socket(&value)?;
                socket = true;
                continue;
            }
            if let Some(value) = option_value(&argument, "--system-path", &mut args)? {
                // The system directory holds every tool's caches. Krusty owns only its child,
                // so startup GC cannot delete a sibling `v*` tree.
                let owned = PathBuf::from(value).join("krusty").join("deps");
                set_cache_dir(&mut options.deps_cache_dir, owned)?;
                continue;
            }
            if let Some(value) = option_value(&argument, "--log-level", &mut args)? {
                log_level_arg = Some(value);
                continue;
            }
            if let Some(value) = option_value(&argument, "--log-category", &mut args)? {
                log_category_args.push(value);
                continue;
            }
            match argument.as_str() {
                "--stdio" => stdio = true,
                "--client" => client = true,
                "--multi-client" => multi_client = true,
                "--dev" => options.dev = true,
                "-cp" | "-classpath" | "-class-path" => {
                    let value = required_value(&argument, &mut args)?;
                    options.classpath.extend(std::env::split_paths(&value));
                }
                "-jdk-home" => {
                    let value = required_value("-jdk-home", &mut args)?;
                    options.jdk_home = Some(PathBuf::from(value));
                }
                "-no-jdk" => options.no_jdk = true,
                "-deps-cache-dir" => {
                    let value = required_value("-deps-cache-dir", &mut args)?;
                    set_cache_dir(&mut options.deps_cache_dir, PathBuf::from(value))?;
                }
                "-deps-cache-max-age-days" => {
                    let value = required_value("-deps-cache-max-age-days", &mut args)?;
                    options.deps_cache_max_age_days = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid -deps-cache-max-age-days '{value}'"))?,
                    );
                }
                "-deps-cache-max-bytes" => {
                    let value = required_value("-deps-cache-max-bytes", &mut args)?;
                    options.deps_cache_max_bytes = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid -deps-cache-max-bytes '{value}'"))?,
                    );
                }
                "-deps-sources" => options.deps_sources = Some(true),
                "-no-deps-sources" => options.deps_sources = Some(false),
                _ => match krusty_cli::cli::language_argument(&argument) {
                    Some(LanguageArgument::Alone) => options.language_arguments.push(argument),
                    Some(LanguageArgument::WithValue) => {
                        let value = required_value(&argument, &mut args)?;
                        options.language_arguments.extend([argument, value]);
                    }
                    None => return Err(format!("unsupported option '{argument}'")),
                },
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
        // Validated for launch compatibility and then dropped. Nothing in the server reads a
        // logging owner, so the parsed selection is not part of the process configuration.
        let log = select_log(log_level_arg, &log_category_args, env)?;
        let _ = (log.level, log.categories.len());
        // The language flags are refused here, at startup, exactly as the command line refuses
        // them, rather than when the first analysis runs.
        let language =
            krusty_cli::cli::language_settings(options.language_arguments.iter().cloned());
        if !language.errors.is_empty() {
            return Err(language.errors.join("\n"));
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

    /// The language settings the explicit LSP flags select on their own, for analyses without a
    /// project module.
    pub fn language_settings(&self) -> LanguageSettings {
        krusty_cli::cli::language_settings(self.language_arguments.iter().cloned()).settings
    }

    /// The kotlinc language arguments given to the server. They apply after a module's own
    /// arguments, so an explicit flag overrides the project's choice as it would on a command
    /// line.
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
        return required_value(name, args).map(Some);
    }
    Ok(None)
}

fn required_value(name: &str, args: &mut impl Iterator<Item = String>) -> Result<String, String> {
    let value = args
        .next()
        .ok_or_else(|| format!("{name} requires a value"))?;
    // A separate token beginning with `-` is ambiguous with an option. Reject it instead of
    // swallowing an unknown/future option as a path or setting. `--name=-value` remains the
    // explicit spelling for long options when a leading dash really is part of the value.
    if value.is_empty() || value.starts_with('-') {
        return Err(format!("{name} requires a value"));
    }
    Ok(value)
}

fn set_cache_dir(slot: &mut Option<PathBuf>, path: PathBuf) -> Result<(), String> {
    if slot.is_some() {
        return Err("pass either --system-path or -deps-cache-dir".to_string());
    }
    if path.as_os_str().is_empty() {
        return Err("cache directory requires a non-empty path".to_string());
    }
    *slot = Some(path);
    Ok(())
}

fn parse_socket(value: &str) -> Result<(), String> {
    let Some((host, port)) = value.rsplit_once(':') else {
        return Err(socket_error(value));
    };
    if host.is_empty() || port.parse::<u16>().is_err() {
        return Err(socket_error(value));
    }
    Ok(())
}

fn socket_error(value: &str) -> String {
    format!("'{value}' is not a valid socket. Expected <host>:<port>")
}

struct LogSelection {
    level: LogLevel,
    categories: Vec<(String, LogLevel)>,
}

fn select_log(
    level_from_args: Option<String>,
    categories_from_args: &[String],
    env: &mut impl FnMut(&str) -> Option<String>,
) -> Result<LogSelection, String> {
    let level = if let Some(value) = level_from_args {
        LogLevel::parse(&value)?
    } else if let Some(value) = env("KOTLIN_LSP_LOG_LEVEL") {
        LogLevel::parse(&value)?
    } else {
        LogLevel::Info
    };
    let mut categories = Vec::new();
    if !categories_from_args.is_empty() {
        for value in categories_from_args {
            push_one_category(&mut categories, value)?;
        }
    } else if let Some(value) = env("KOTLIN_LSP_LOG_CATEGORIES") {
        push_one_category(&mut categories, &value)?;
    }
    Ok(LogSelection { level, categories })
}

fn push_one_category(categories: &mut Vec<(String, LogLevel)>, value: &str) -> Result<(), String> {
    if value.contains(',') {
        return Err(category_level_error(value));
    }
    let Some((category, level)) = value.split_once(':') else {
        return Err(category_level_error(value));
    };
    if category.is_empty()
        || level.is_empty()
        || category.trim() != category
        || level.trim() != level
    {
        return Err(category_level_error(value));
    }
    categories.push((category.to_string(), LogLevel::parse(level)?));
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
      Kotlin LSP system directory. Krusty's dependency cache is <path>/krusty/deps.
      This is the stdio launch subset: socket, client, and multiclient modes are not
      implemented, and with no transport flag the server still speaks stdio.
  --log-level <LEVEL>
      TRACE, DEBUG, INFO, WARNING, ERROR, OFF, or ALL. Case-sensitive.
      Accepted and validated for launch compatibility. It does not configure a logger.
      KOTLIN_LSP_LOG_LEVEL supplies the value when the option is absent.
  --log-category <category:LEVEL>
      One category and level. Repeat the option for several categories.
      Accepted and validated for launch compatibility. It does not configure a logger.
      KOTLIN_LSP_LOG_CATEGORIES supplies one category:LEVEL when the option is absent.
  -cp, -classpath, -class-path <path>
  -jdk-home <path>
  -no-jdk
  -deps-cache-dir <path>
  -deps-cache-max-age-days <days>
  -deps-cache-max-bytes <bytes>
  -deps-sources, -no-deps-sources
  --dev
  -h, --help
      kotlinc language arguments are accepted as well: -language-version, -api-version,
      -progressive, -XXLanguage:, and the -X arguments that switch language features.
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
    use krusty::language_version::LanguageVersion;

    fn parse(args: &[&str]) -> Result<LspOptions, String> {
        parse_env(args, |_| None)
    }

    fn invalid(args: &[&str]) -> String {
        match LspOptions::parse_with(args.iter().map(|argument| argument.to_string()), |_| None) {
            Err(OptionsError::Invalid(text)) => text,
            other => panic!("expected an invalid-option error, got {other:?}"),
        }
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
            "-Xname-based-destructuring=only-syntax",
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
        assert!(options
            .language_settings()
            .features
            .has("NameBasedDestructuring"));
        let options = parse(&[
            "-Xname-based-destructuring=only-syntax",
            "-Xname-based-destructuring=disable",
            "-language-version",
            "2.3",
        ])
        .unwrap();
        let language = options.language_settings();
        assert!(!language.features.has("NameBasedDestructuring"));
        assert_eq!(language.language_version, LanguageVersion::V2_3);
        // The server's flags apply after a module's own arguments, as on a command line.
        let mut module = vec!["-Xname-based-destructuring=only-syntax".to_string()];
        module.extend_from_slice(options.language_arguments());
        let layered = krusty_cli::cli::language_settings(module).settings;
        assert!(!layered.features.has("NameBasedDestructuring"));
        assert!(!layered
            .features
            .has("EnableNameBasedDestructuringShortForm"));
        // A comma is part of a `-XXLanguage` feature name, as in kotlinc; a feature krusty does not
        // implement is refused at startup.
        assert!(parse(&["-XXLanguage:+ErrorAboutDataClassCopyVisibilityChange"]).is_err());
        assert!(parse(&["-language-version"]).is_err());
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
    fn stdio_launch_keeps_krusty_cache_under_the_system_path() {
        let options = parse(&[
            "--stdio",
            "--system-path=/tmp/kotlin-lsp",
            "--log-level=DEBUG",
            "--log-category=com.example:TRACE",
            "--log-category=indexing:INFO",
            "--socket=127.0.0.1:9999",
        ])
        .unwrap();
        assert_eq!(
            options.deps_cache_dir(),
            Some(Path::new("/tmp/kotlin-lsp/krusty/deps"))
        );
        let log = select_log(
            Some("DEBUG".to_string()),
            &["com.example:TRACE".to_string(), "indexing:INFO".to_string()],
            &mut |_| None,
        )
        .unwrap();
        assert_eq!(log.level, LogLevel::Debug);
        assert_eq!(
            log.categories,
            [
                ("com.example".to_string(), LogLevel::Trace),
                ("indexing".to_string(), LogLevel::Info),
            ]
        );
    }

    #[test]
    fn log_options_fall_back_to_the_kotlin_lsp_environment() {
        let from_env = select_log(None, &[], &mut |key| match key {
            "KOTLIN_LSP_LOG_LEVEL" => Some("WARNING".to_string()),
            "KOTLIN_LSP_LOG_CATEGORIES" => Some("resolve:ERROR".to_string()),
            _ => None,
        })
        .unwrap();
        assert_eq!(from_env.level, LogLevel::Warning);
        assert_eq!(
            from_env.categories,
            [("resolve".to_string(), LogLevel::Error)]
        );

        let overridden = select_log(
            Some("OFF".to_string()),
            &["editor:DEBUG".to_string()],
            &mut |_| Some("TRACE".to_string()),
        )
        .unwrap();
        assert_eq!(overridden.level, LogLevel::Off);
        assert_eq!(
            overridden.categories,
            [("editor".to_string(), LogLevel::Debug)]
        );
        assert!(parse(&[
            "--stdio",
            "--log-level",
            "OFF",
            "--log-category",
            "editor:DEBUG",
        ])
        .is_ok());
    }

    #[test]
    fn kotlin_lsp_launch_rejects_a_bad_level_category_or_socket() {
        assert_eq!(
            invalid(&["--stdio", "--log-level=debug"]),
            "'debug' is not a valid log level. Supported values: TRACE, DEBUG, INFO, \
             WARNING, ERROR, OFF, ALL (case-sensitive)"
        );
        assert_eq!(
            invalid(&["--stdio", "--log-category=:DEBUG"]),
            "':DEBUG' is not a valid category:level. Expected format <category>:<level>, \
             e.g. com.example:DEBUG"
        );
        assert_eq!(
            invalid(&["--stdio", "--log-category= :DEBUG"]),
            "' :DEBUG' is not a valid category:level. Expected format <category>:<level>, \
             e.g. com.example:DEBUG"
        );
        assert_eq!(
            invalid(&["--stdio", "--log-category=com.example:TRACE,indexing:INFO"]),
            "'com.example:TRACE,indexing:INFO' is not a valid category:level. Expected format \
             <category>:<level>, e.g. com.example:DEBUG"
        );
        assert_eq!(
            invalid(&["--stdio", "--socket", "nope"]),
            "'nope' is not a valid socket. Expected <host>:<port>"
        );
        assert_eq!(
            invalid(&["--socket=9999"]),
            "'9999' is not a valid socket. Expected <host>:<port>"
        );
        assert_eq!(
            invalid(&["--socket=127.0.0.1:65536"]),
            "'127.0.0.1:65536' is not a valid socket. Expected <host>:<port>"
        );
        assert_eq!(
            invalid(&["--socket=127.0.0.1:-1"]),
            "'127.0.0.1:-1' is not a valid socket. Expected <host>:<port>"
        );
        assert!(parse(&["--stdio", "--socket=[::1]:9999"]).is_ok());
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
            invalid(&["--system-path", "/a", "-deps-cache-dir", "/b"]),
            "pass either --system-path or -deps-cache-dir"
        );
        assert_eq!(
            invalid(&["--system-path", ""]),
            "--system-path requires a value"
        );
        assert_eq!(
            invalid(&["--system-path="]),
            "--system-path requires a value"
        );
        assert_eq!(
            invalid(&["--system-path", "--stdio"]),
            "--system-path requires a value"
        );
        for option in [
            "--socket",
            "--system-path",
            "--log-level",
            "--log-category",
            "-cp",
            "-jdk-home",
        ] {
            assert_eq!(
                invalid(&[option, "--not-a-real-option"]),
                format!("{option} requires a value"),
                "{option} must not consume an unknown option as its value"
            );
        }
        for value in ["editor: DEBUG", "editor:DEBUG "] {
            let argument = format!("--log-category={value}");
            assert_eq!(
                invalid(&["--stdio", &argument]),
                format!(
                    "'{value}' is not a valid category:level. Expected format \
                     <category>:<level>, e.g. com.example:DEBUG"
                )
            );
        }
    }

    #[test]
    fn help_is_the_help_variant_and_not_a_startup_error() {
        for args in [["--help"], ["-h"]] {
            match LspOptions::parse(args.iter().map(|argument| argument.to_string())) {
                Err(OptionsError::Help(text)) => assert_eq!(text, help_text()),
                Err(OptionsError::Invalid(text)) => {
                    panic!("help was reported as a startup error: {text}")
                }
                Ok(_) => panic!("help was accepted as a launch"),
            }
        }
    }

    #[test]
    fn system_path_gc_leaves_foreign_version_directories_in_place() {
        let root = std::env::temp_dir().join(format!(
            "krusty-system-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("v0")).unwrap();
        std::fs::create_dir_all(root.join("v2")).unwrap();
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::write(root.join("project").join("keep.txt"), b"keep").unwrap();

        let options = LspOptions::parse([
            "--system-path".to_string(),
            root.to_str().unwrap().to_string(),
        ])
        .unwrap();
        let cache = options.deps_cache_dir().unwrap();
        assert_eq!(cache, root.join("krusty").join("deps"));
        crate::deps_cache::gc(cache, 0, 0, u64::MAX).unwrap();

        assert!(root.join("v0").is_dir());
        assert!(root.join("v2").is_dir());
        assert_eq!(
            std::fs::read(root.join("project").join("keep.txt")).unwrap(),
            b"keep"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
