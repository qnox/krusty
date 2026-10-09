//! kotlinc-compatible command line, so `krusty` can stand in for `kotlinc` in a build. The command
//! line is split by kotlinc's own rules and argument table ([`crate::kotlinc_arguments`]); this
//! module applies the arguments krusty implements and accepts source files **or directories**. A
//! kotlinc argument krusty does not implement is a fatal error: the same command line must never
//! compile successfully into something other than what it asks kotlinc for.

use std::collections::BTreeMap;
use std::path::PathBuf;

use krusty::jvm::compilation_inputs::JvmCompilationInputInventory;
use krusty::jvm::ir_emit::{JvmDefaultMode, LambdaModes};
use krusty::kotlin_version::KotlinVersion;
use krusty::language_settings::LanguageSettings;
use krusty::plugins::cli::PluginConfig;
use krusty::plugins::registry::{Activation, NativePlugins, PluginRegistry};

use crate::kotlinc_arguments::{self, Catalog, Disposition, Lifecycle, Origin};

mod arguments;

use arguments::{
    apply, collect_sources, finish_language_settings, take_reference_version, unsupported,
    ParsedSettings,
};

/// Stable diagnostic names exposed through kotlinc's `-Xwarning-level` contract. A name enters
/// this registry only when krusty can emit that diagnostic; accepting any other spelling would
/// promise a policy the compiler cannot apply.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WarningName {
    DeprecatedCliArg,
    DeprecatedLanguageVersion,
    ExperimentalLanguageVersion,
    RedundantCliArg,
    RemovedCliArg,
}

impl WarningName {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "DEPRECATED_CLI_ARG" => Self::DeprecatedCliArg,
            "DEPRECATED_LANGUAGE_VERSION" => Self::DeprecatedLanguageVersion,
            "EXPERIMENTAL_LANGUAGE_VERSION" => Self::ExperimentalLanguageVersion,
            "REDUNDANT_CLI_ARG" => Self::RedundantCliArg,
            "REMOVED_CLI_ARG" => Self::RemovedCliArg,
            _ => return None,
        })
    }
}

/// The three severities accepted by kotlinc's warning-level option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WarningLevel {
    Error,
    Warning,
    Disabled,
}

impl WarningLevel {
    fn parse(level: &str) -> Option<Self> {
        Some(match level {
            "error" => Self::Error,
            "warning" => Self::Warning,
            "disabled" => Self::Disabled,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct WarningPolicy {
    configured: BTreeMap<WarningName, WarningLevel>,
}

impl WarningPolicy {
    fn configure(&mut self, value: &str) -> Result<(), String> {
        let Some((name, severity)) = value.split_once(':') else {
            return Err(format!(
                "invalid value '{value}' for -Xwarning-level: expected <NAME>:<error|warning|disabled>"
            ));
        };
        let Some(name) = WarningName::parse(name) else {
            let name = value.split_once(':').map_or(value, |(name, _)| name);
            return Err(format!("warning with name \"{name}\" does not exist"));
        };
        let Some(severity) = WarningLevel::parse(severity) else {
            return Err(format!(
                "invalid severity '{severity}' in -Xwarning-level={value}; supported severities: error, warning, disabled"
            ));
        };
        if self.configured.contains_key(&name) {
            let name = value.split_once(':').map_or(value, |(name, _)| name);
            return Err(format!(
                "warning with name \"{name}\" has already been configured"
            ));
        }
        self.configured.insert(name, severity);
        Ok(())
    }

    pub fn level(&self, name: WarningName) -> WarningLevel {
        self.configured
            .get(&name)
            .copied()
            .unwrap_or(WarningLevel::Warning)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliWarning {
    /// `None` for a warning kotlinc reports without a diagnostic name, which `-Xwarning-level`
    /// cannot reach.
    pub name: Option<WarningName>,
    pub message: String,
}

pub struct Options {
    /// Output directory or `.jar` (kotlinc `-d`).
    pub dest: PathBuf,
    /// Classpath entries (dirs/jars).
    pub classpath: Vec<PathBuf>,
    /// Classpath entries whose Kotlin `internal` declarations are visible to this module.
    pub friend_paths: Vec<PathBuf>,
    /// Batch-compilable Kotlin and Java source inputs (directories already expanded). Java files
    /// contribute source headers to Kotlin analysis; their executable bodies remain javac's work.
    pub sources: Vec<String>,
    /// Module name → `<module>.kotlin_module` (kotlinc `-module-name`, default `main`).
    pub module_name: String,
    /// Standard per-compilation language/API versions and their finalized feature baseline.
    pub language_settings: LanguageSettings,
    /// Inputs krusty skips (Kotlin scripts), each reported once.
    pub ignored: Vec<String>,
    /// kotlinc's own command-line syntax errors (`Invalid argument: -foo`), in its words. Any one
    /// stops the invocation before compiling, exactly as kotlinc does.
    pub argument_errors: Vec<String>,
    /// kotlinc's command-line syntax warnings (an unknown `-X` flag, a deprecated spelling, an
    /// unreadable argfile), in its words; compilation is unchanged.
    pub argument_warnings: Vec<String>,
    /// Named CLI warnings and their configured severity. These are evaluated before compilation;
    /// an `error` level fails the invocation and `disabled` removes the diagnostic entirely.
    pub warning_policy: WarningPolicy,
    pub warnings: Vec<CliWarning>,
    /// Invalid or explicitly requested-but-unemittable options. The driver reports these and exits
    /// before compilation rather than silently producing a different artifact.
    pub errors: Vec<String>,
    /// `-version` / `-help` requested (handled before compiling).
    pub print_version: bool,
    pub print_help: bool,
    /// `-jdk-home <dir>`: the JDK whose bootclasspath (`lib/modules` on JDK 9+, `jre/lib/rt.jar`
    /// on JDK 8) seeds the compile classpath.
    pub jdk_home: Option<PathBuf>,
    /// `-no-stdlib`: do not add the Kotlin standard library to the compile classpath.
    pub no_stdlib: bool,
    /// `-no-reflect`: do not add Kotlin reflection to the compile classpath.
    pub no_reflect: bool,
    /// `-no-jdk`: do NOT add the platform JDK to the classpath (kotlinc semantics).
    pub no_jdk: bool,
    /// `-jvm-default <mode>` (or legacy `-Xjvm-default <mode>`): how an interface's members with
    /// bodies are realized on the JVM.
    pub jvm_default: JvmDefaultMode,
    /// `-java-parameters`: write a `MethodParameters` attribute naming each declared parameter, so a
    /// reflection-driven framework can read the names without a debug table.
    pub java_parameters: bool,
    /// Independently selected `-Xlambdas` / `-Xsam-conversions` strategies.
    pub lambda_modes: LambdaModes,
    /// `-Xno-param-assertions`: omit the `Intrinsics.checkNotNullParameter` guards kotlinc emits at
    /// the entry of every function reachable from Java.
    pub no_param_assertions: bool,
    /// `-Xno-call-assertions`: omit the not-null assertions on platform-typed values coming back
    /// from Java. Recorded, but krusty emits no such assertions yet, so it changes no bytes today.
    pub no_call_assertions: bool,
    /// `-jvm-target <v>`: the emitted class-file major version (kotlinc maps `1.8`→52, `9`→53, …,
    /// `25`→69). `None` keeps krusty's default (Java 8 / major 52), which runs on the test JDK.
    pub jvm_target_major: Option<u16>,
    /// The compiler-plugin switches (`-Xplugin`, `-P`, `-Xcompiler-plugin`), resolved against krusty's
    /// extension registry by [`Options::resolve_plugins`].
    pub plugins: PluginConfig,
    /// `-Xkotlin-reference-version=<major.minor.patch>`: the supported kotlinc release whose output
    /// (diagnostic wording, class-file details) this compilation reproduces. `None` leaves the
    /// choice to `KRUSTY_LANGUAGE_VERSION`, then the newest supported release.
    pub kotlin_reference_version: Option<KotlinVersion>,
    /// `-Xmetadata-version <major.minor>`: the artifact stamp written as every `@kotlin.Metadata`
    /// `mv` and as the `.kotlin_module` header version (`[X, Y, 0]`). This does not select language
    /// semantics. `None` keeps the default stamp, the implemented language version.
    pub metadata_version: Option<[i32; 3]>,
    /// `-Xexplicit-api=strict|warning|disable`: kotlinc's explicit API mode, applied to the
    /// language settings once they are built.
    pub explicit_api: Option<String>,
    /// `-opt-in=<fq name>[,<fq name>…]` (also `-opt-in <value>`, repeatable): requirement markers
    /// accepted module-wide, applied to the language settings once they are built.
    pub opt_in: Vec<String>,
    /// `-Xsuppress-version-warnings`: omit the deprecated and experimental language/API warnings.
    /// Redundant feature arguments stay reported.
    pub suppress_version_warnings: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            dest: PathBuf::from("krusty-out"),
            classpath: Vec::new(),
            friend_paths: Vec::new(),
            sources: Vec::new(),
            module_name: "main".to_string(),
            language_settings: LanguageSettings::default(),
            ignored: Vec::new(),
            argument_errors: Vec::new(),
            argument_warnings: Vec::new(),
            warning_policy: WarningPolicy::default(),
            warnings: Vec::new(),
            errors: Vec::new(),
            print_version: false,
            print_help: false,
            jdk_home: None,
            no_stdlib: false,
            no_reflect: false,
            no_jdk: false,
            jvm_target_major: None,
            kotlin_reference_version: None,
            metadata_version: None,
            jvm_default: JvmDefaultMode::default(),
            java_parameters: false,
            lambda_modes: LambdaModes::default(),
            no_param_assertions: false,
            no_call_assertions: false,
            plugins: PluginConfig::default(),
            explicit_api: None,
            opt_in: Vec::new(),
            suppress_version_warnings: false,
        }
    }
}

/// Map a kotlinc `-jvm-target` value to the class-file major version it produces. `1.6`/`1.8` are the
/// legacy dotted spellings; `9`+ are bare. Unknown values yield `None` (krusty keeps its default).
pub fn jvm_target_to_major(v: &str) -> Option<u16> {
    match v {
        "1.6" | "6" => Some(50),
        "1.7" | "7" => Some(51),
        "1.8" | "8" => Some(52),
        _ => v
            .parse::<u16>()
            .ok()
            .filter(|&n| (9..=99).contains(&n))
            .map(|n| n + 44),
    }
}

/// Parse argv (already skipping the program name) the way kotlinc does: `@file` argfiles expand
/// inline, the reference release's argument table decides what each token is, and krusty then
/// applies the arguments it implements.
pub fn parse(argv: impl IntoIterator<Item = String>) -> Options {
    let mut opts = Options::default();
    let mut argfile_problems = Vec::new();
    let expanded = kotlinc_arguments::argfile::expand(argv, &mut argfile_problems);
    let arguments = take_reference_version(expanded, &mut opts);
    let reference_version = opts.kotlin_reference_version.unwrap_or_else(|| {
        krusty::kotlin_version::configured_target().unwrap_or_else(|_| KotlinVersion::newest())
    });
    let catalog = Catalog::for_version(reference_version)
        .expect("every supported reference release has an argument table");
    let tokenized = kotlinc_arguments::tokenize(catalog, arguments, argfile_problems);
    opts.argument_errors = tokenized.errors();
    opts.argument_warnings = tokenized.warnings();

    let mut parsed = ParsedSettings::default();
    for occurrence in &tokenized.occurrences {
        // kotlinc parses a removed argument only to warn that it has no effect.
        if occurrence.spec.origin == Origin::Removed {
            continue;
        }
        match kotlinc_arguments::disposition::of(&occurrence.spec.name) {
            Some(Disposition::Applied) => apply(&mut opts, &mut parsed, occurrence),
            Some(Disposition::Inert) => {}
            Some(Disposition::Unsupported) | None => opts.errors.push(unsupported(occurrence)),
        }
    }
    for source in &tokenized.free {
        collect_sources(source, &mut opts.sources, &mut opts.ignored);
    }
    opts.plugins.finish();
    opts.errors.append(&mut opts.plugins.errors);
    let lifecycle = kotlinc_arguments::lifecycle_warnings(&tokenized, reference_version)
        .into_iter()
        .map(|(lifecycle, message)| CliWarning {
            name: Some(match lifecycle {
                Lifecycle::Deprecated => WarningName::DeprecatedCliArg,
                Lifecycle::Removed => WarningName::RemovedCliArg,
            }),
            message: kotlinc_arguments::render(&message),
        })
        .collect();
    finish_language_settings(&mut opts, parsed, catalog, lifecycle);
    opts
}

/// What krusty does with the compiler plugins a command line requests.
pub struct PluginResolution {
    /// The native extensions to run in place of the requested plugin jars.
    pub native: NativePlugins,
    /// Printed as `info:` — a plugin krusty substitutes with its own implementation.
    pub notes: Vec<String>,
    /// Printed as `error:` — any one fails the compilation before it starts.
    pub errors: Vec<String>,
}

impl Options {
    /// Resolve the requested compiler plugins against krusty's extension registry, which alone knows
    /// which plugins krusty implements. A jar that does not exist is kotlinc's own error; a plugin
    /// krusty can neither substitute nor host is an error too, because ignoring it would compile
    /// silently wrong code. This driver runs no codegen host, so a KSP request is refused as well.
    pub fn resolve_plugins(&self, classpath: &[PathBuf]) -> PluginResolution {
        let missing = plugin_jar_problems(&self.plugins);
        if !missing.is_empty() {
            return PluginResolution {
                native: NativePlugins::none(),
                notes: Vec::new(),
                errors: missing,
            };
        }
        let classpath = classpath
            .iter()
            .map(|entry| entry.display().to_string())
            .collect::<Vec<_>>();
        let resolved = PluginRegistry::with_builtins().resolve(&Activation {
            config: &self.plugins,
            classpath: &classpath,
            module_name: &self.module_name,
            codegen_host: false,
        });
        let (errors, notes) = resolved
            .diagnostics
            .iter()
            .partition::<Vec<_>, _>(|diagnostic| diagnostic.is_error());
        PluginResolution {
            native: resolved.native,
            notes: notes.iter().map(|note| note.message()).collect(),
            errors: errors.iter().map(|error| error.message()).collect(),
        }
    }
}

/// kotlinc refuses a plugin jar that does not exist, in these words (measured on 2.4.20): the legacy
/// syntax per entry, the modern one per plugin classpath.
fn plugin_jar_problems(plugins: &PluginConfig) -> Vec<String> {
    let exists = |jar: &String| std::path::Path::new(jar).exists();
    let legacy = plugins
        .plugin_jars
        .iter()
        .filter(|jar| !exists(jar))
        .map(|jar| format!("plugin classpath entry points to a non-existent location: {jar}"));
    let modern = plugins
        .compiler_plugins
        .iter()
        .filter(|plugin| !plugin.classpath.iter().all(exists))
        .map(|plugin| {
            format!(
                "no plugins found in given classpath: {}",
                plugin.classpath.join(",")
            )
        });
    legacy.chain(modern).collect()
}

impl Options {
    /// The classpath to drive resolution with: the user's `-cp` entries plus kotlinc's implicit
    /// standard library and JDK modules, unless their corresponding `-no-*` option disables them.
    /// Kept out of `parse` so parsing stays env-independent.
    pub fn effective_classpath(&self) -> Result<Vec<PathBuf>, String> {
        let mut cp = self.classpath.clone();
        if !self.no_stdlib {
            let stdlib = krusty::jvm::kotlin_stdlib_jar().ok_or_else(|| {
                "cannot locate kotlin-stdlib.jar; configure a Kotlin distribution or pass -no-stdlib"
                    .to_string()
            })?;
            if !cp.contains(&stdlib) {
                cp.push(stdlib);
            }
            if !self.no_reflect {
                let reflect = krusty::jvm::kotlin_dist_jar("kotlin-reflect.jar").ok_or_else(|| {
                    "cannot locate kotlin-reflect.jar in the selected Kotlin distribution; pass -no-reflect to disable it"
                        .to_string()
                })?;
                if !cp.contains(&reflect) {
                    cp.push(reflect);
                }
            }
        }
        if !self.no_jdk {
            cp = JvmCompilationInputInventory::from_explicit_classpath_and_jdk(
                &cp,
                self.jdk_home.as_deref(),
            )
            .effective_classpath()
            .to_vec();
        }
        Ok(cp)
    }
}

/// Something wrong with a user-supplied `-cp` entry, found before analysis opens it.
///
/// The classpath reader drops an entry it cannot open WITHOUT saying so, and the compile then reads
/// as a resolution failure ("unresolved reference 'kotlinx'") that nothing in the output ties to
/// the jar. kotlinc warns in both cases and continues; so does krusty, in kotlinc's words for the
/// missing case, so the cause is on stderr the moment it happens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClasspathEntryProblem {
    /// The path does not exist. kotlinc: `warning: classpath entry points to a non-existent location`.
    Missing(PathBuf),
    /// The path exists but cannot be read, or names an archive that does not open as one.
    Unreadable { path: PathBuf, reason: String },
}

impl std::fmt::Display for ClasspathEntryProblem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(path) => write!(
                formatter,
                "warning: classpath entry points to a non-existent location: {}",
                path.display()
            ),
            Self::Unreadable { path, reason } => write!(
                formatter,
                "warning: cannot read classpath entry {}: {reason}",
                path.display()
            ),
        }
    }
}

/// Classify every user-supplied classpath entry. A directory only has to be listable; an archive
/// (`.jar`/`.zip`) has to open as one; any other file (a JDK `lib/modules` jimage, say) only has to
/// open — its format is the reader's business.
pub fn classpath_entry_problems(entries: &[PathBuf]) -> Vec<ClasspathEntryProblem> {
    entries
        .iter()
        .filter_map(|path| {
            if !path.exists() {
                return Some(ClasspathEntryProblem::Missing(path.clone()));
            }
            let reason = if path.is_dir() {
                std::fs::read_dir(path).err().map(|error| error.to_string())
            } else {
                match std::fs::File::open(path) {
                    Err(error) => Some(error.to_string()),
                    Ok(file) => {
                        let is_archive = path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                extension.eq_ignore_ascii_case("jar")
                                    || extension.eq_ignore_ascii_case("zip")
                            });
                        if is_archive {
                            zip::ZipArchive::new(file)
                                .err()
                                .map(|error| format!("not a readable archive ({error})"))
                        } else {
                            None
                        }
                    }
                }
            };
            reason.map(|reason| ClasspathEntryProblem::Unreadable {
                path: path.clone(),
                reason,
            })
        })
        .collect()
}

/// krusty's release version. Injected at build time via the `KRUSTY_VERSION` env var; the `just`
/// release recipe sets it to `<max-Kotlin-reference-version>-build.<n>` (e.g. 2.4.20-build.3, a
/// SemVer prerelease so builds stay strictly ordered). Falls back to the crate version for a plain
/// `cargo build`, so local dev builds still report something sensible.
pub const VERSION: &str = match option_env!("KRUSTY_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Kotlin reference versions this build is validated against / supports, injected at build time from
/// the `kotlin-versions` manifest. Lets `krusty -version` advertise its supported Kotlins.
pub const KOTLIN_SUPPORT: &str = match option_env!("KRUSTY_KOTLIN_SUPPORT") {
    Some(v) => v,
    None => "unknown (dev build)",
};

/// The manifest's reference versions, for an error that names what `-Xkotlin-reference-version`
/// accepts.
fn supported_kotlin_versions() -> String {
    KotlinVersion::supported()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Human-facing `-version` output.
pub fn version_line() -> String {
    format!(
        "krusty {VERSION} (kotlinc-compatible Kotlin\u{2192}JVM compiler PoC)\nsupported Kotlin: {KOTLIN_SUPPORT}"
    )
}

pub const HELP: &str = "\
usage: krusty [options] <sources>

krusty is a memory-lean Kotlin\u{2192}JVM compiler PoC that aims to be a drop-in for kotlinc on the
supported language subset (kotlinc-equivalent ABI, verified by a differential harness).

Common options (kotlinc-compatible):
  -d <dir|jar>          destination for generated .class files (a directory or a .jar)
  -classpath / -cp <p>  classpath entries (dirs and .jars), ':'-separated
  -Xfriend-paths=<p,…>  classpath entries whose internal declarations are visible, ','-separated
  -module-name <name>   name of the generated <name>.kotlin_module (default: main)
  -jvm-target <v>        class-file version to emit (1.8→v52, 9→v53, …, 25→v69; default v52)
  -language-version <v>  source semantics to compile (accepted/default level follows kotlinc release)
  -api-version <v>       Kotlin API surface available to source (defaults to language version)
  -Xmetadata-version <v> internal artifact stamp for @kotlin.Metadata and the
                         .kotlin_module header (2.0–2.4; does not change semantics)
  -version              print version and exit
  -jvm-default <mode>   interface default-method strategy: enable | no-compatibility
                        (legacy -Xjvm-default=all | all-compatibility)
  -Xno-param-assertions omit JVM entry guards for non-null parameters
  -Xno-call-assertions  omit JVM assertions on platform-typed call results
  -Xlambdas=indy        emit lambdas through LambdaMetafactory (class is not supported)
  -Xsam-conversions=indy emit SAM conversions through LambdaMetafactory
  -Xplugin=<jar>,<jar>  compiler plugins; kotlinx.serialization runs as krusty's native pass,
                        any other plugin is an error (krusty cannot run FIR/IR plugin jars)
  -P plugin:<id>:<key>=<value>
                        pass an option to a plugin
  -Xkotlin-reference-version=<v>
                        reproduce this supported kotlinc release (default: the newest)
  -help                 print this help and exit

Sources may be .kt files or directories (scanned recursively). Kotlin scripts are not yet compiled.
Arguments are parsed by kotlinc's rules for the selected release. A kotlinc argument krusty does not
implement is an error, as are invalid values; an unknown option is kotlinc's own error or warning.
krusty never compiles a command line into anything other than what kotlinc would produce.";

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::jvm::ir_emit::LambdaMode;
    use krusty::language_version::LanguageVersion;
    use krusty::plugins::registry::PluginDiagnostic;

    fn parse_args(args: &[&str]) -> Options {
        parse(args.iter().map(|s| s.to_string()))
    }

    /// `-progressive` enables the selected release's progressive features, and an explicit
    /// `-XXLanguage` still overrides one wherever it appears, as in kotlinc's
    /// `configureLanguageFeatures`.
    #[test]
    fn progressive_mode_enables_the_releases_progressive_features() {
        let feature = "AllowEagerSupertypeAccessibilityChecks";
        let enabled = |args: &[&str]| {
            let o = parse_args(args);
            assert_eq!(o.errors, Vec::<String>::new());
            assert!(o.warnings.is_empty());
            o.language_settings.features.has(feature)
        };
        assert!(!enabled(&["-Xkotlin-reference-version=2.4.20", "f.kt"]));
        assert!(enabled(&[
            "-Xkotlin-reference-version=2.4.20",
            "-progressive",
            "f.kt"
        ]));
        // 2.4.10 has no such progressive feature.
        assert!(!enabled(&[
            "-Xkotlin-reference-version=2.4.10",
            "-progressive",
            "f.kt"
        ]));
        assert!(!enabled(&[
            "-Xkotlin-reference-version=2.4.20",
            "-XXLanguage:-AllowEagerSupertypeAccessibilityChecks",
            "-progressive",
            "f.kt",
        ]));
    }

    /// `-Xkotlin-reference-version` selects a supported release and refuses any other, rather than
    /// silently reproducing a different kotlinc than the one asked for.
    #[test]
    fn the_reference_version_flag_accepts_only_supported_releases() {
        let parsed = parse_args(&["-Xkotlin-reference-version=2.4.10", "x.kt"]);
        assert_eq!(
            parsed.kotlin_reference_version,
            Some(KotlinVersion::V2_4_10)
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        for bad in ["2.3.0", "2.4", "latest", ""] {
            let parsed = parse_args(&[&format!("-Xkotlin-reference-version={bad}"), "x.kt"]);
            assert_eq!(parsed.kotlin_reference_version, None, "{bad:?}");
            assert_eq!(parsed.errors.len(), 1, "{bad:?}: {:?}", parsed.errors);
        }
    }

    /// Both assertion flags must reach `Options`, not fall through to `ignored`: a missing match arm
    /// is silent, and the build would keep emitting guards it asked to have removed.
    #[test]
    fn assertion_flags_are_read_not_ignored() {
        let parsed = parse_args(&["-Xno-param-assertions", "-Xno-call-assertions", "x.kt"]);
        assert!(parsed.no_param_assertions);
        assert!(parsed.no_call_assertions);
        assert!(
            !parsed
                .ignored
                .iter()
                .any(|entry| entry.contains("assertions")),
            "neither flag may be reported as unsupported: {:?}",
            parsed.ignored
        );
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);

        let default = parse_args(&["x.kt"]);
        assert!(!default.no_param_assertions);
        assert!(!default.no_call_assertions);
    }

    /// `-Xconsistent-data-class-copy-visibility` is genuinely modeled (a `LangFeatures` toggle the
    /// backend reads), so it must land in `features`, never in `ignored` — the Bazel worker refuses
    /// any request whose kotlinc options parse to a non-empty `ignored`.
    #[test]
    fn consistent_copy_visibility_is_modeled_not_ignored() {
        let parsed = parse_args(&["-Xconsistent-data-class-copy-visibility", "x.kt"]);
        assert!(
            parsed.ignored.is_empty(),
            "the flag must not be reported as unsupported: {:?}",
            parsed.ignored
        );
        assert!(parsed
            .language_settings
            .features
            .has("DataClassCopyRespectsConstructorVisibility"));
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
    }

    /// `-Xcontext-parameters` is already on at this language level. Recognizing the spelling is
    /// what keeps it out of `ignored`; the worker refuses a request with anything there.
    #[test]
    fn context_parameters_flag_is_modeled_not_ignored() {
        let parsed = parse_args(&["-Xcontext-parameters", "x.kt"]);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(parsed.language_settings.features.has("ContextParameters"));
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
    }

    /// Explicit backing fields are on at language level 2.4. The `-X` spelling is redundant,
    /// as it is for kotlinc, and must still be modeled so it never lands in `ignored`.
    #[test]
    fn explicit_backing_fields_flag_is_modeled_not_ignored() {
        assert!(parse_args(&["x.kt"])
            .language_settings
            .features
            .has("ExplicitBackingFields"));
        let parsed = parse_args(&["-Xexplicit-backing-fields", "x.kt"]);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(parsed
            .language_settings
            .features
            .has("ExplicitBackingFields"));
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
    }

    #[test]
    fn name_based_destructuring_complete_is_modeled_not_ignored() {
        let parsed = parse_args(&["-Xname-based-destructuring=complete", "x.kt"]);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(parsed
            .language_settings
            .features
            .has("NameBasedDestructuring"));
        assert!(parsed
            .language_settings
            .features
            .has("EnableNameBasedDestructuringShortForm"));
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
    }

    /// `indy` is what krusty emits, so asking for it is honored silently. Any other value asks for a
    /// shape krusty cannot emit and must be reported — compiling `class` as `indy` would hand the
    /// build a different set of class files than it asked for.
    #[test]
    fn both_lambda_strategies_are_accepted_and_unknown_values_fail() {
        for flag in ["-Xlambdas=indy", "-Xsam-conversions=indy"] {
            let parsed = parse_args(&[flag, "x.kt"]);
            assert!(
                parsed.ignored.is_empty(),
                "{flag} matches what krusty emits: {:?}",
                parsed.ignored
            );
            assert!(parsed.errors.is_empty(), "{flag}: {:?}", parsed.errors);
            assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
        }
        // `class` selects the synthetic-lambda-class strategy, which the emitter now implements;
        // it must parse into that mode rather than be reported.
        for flag in ["-Xlambdas=class", "-Xsam-conversions=class"] {
            let parsed = parse_args(&[flag, "x.kt"]);
            assert!(parsed.errors.is_empty(), "{flag}: {:?}", parsed.errors);
            let selected = if flag.starts_with("-Xlambdas") {
                parsed.lambda_modes.lambdas
            } else {
                parsed.lambda_modes.sam_conversions
            };
            assert_eq!(selected, LambdaMode::Class, "{flag}");
        }
        let parsed = parse_args(&["-Xlambdas=class", "-Xsam-conversions=indy", "x.kt"]);
        assert_eq!(parsed.lambda_modes.lambdas, LambdaMode::Class);
        assert_eq!(parsed.lambda_modes.sam_conversions, LambdaMode::Indy);
        // A value naming NEITHER strategy still fails: it would otherwise compile as one of them.
        let parsed = parse_args(&["-Xlambdas=nonesuch", "x.kt"]);
        assert!(
            parsed.errors.iter().any(|entry| entry.contains("nonesuch")),
            "an unknown strategy must FAIL the compile: {:?}",
            parsed.errors
        );
    }

    #[test]
    fn jvm_default_mode_is_read_in_both_spellings() {
        use krusty::jvm::ir_emit::JvmDefaultMode;
        // The current spelling.
        for (value, expected) in [
            ("enable", JvmDefaultMode::Enable),
            ("no-compatibility", JvmDefaultMode::NoCompatibility),
        ] {
            assert_eq!(
                parse_args(&[&format!("-jvm-default={value}"), "x.kt"]).jvm_default,
                expected,
                "-jvm-default={value}"
            );
            assert_eq!(
                parse_args(&["-jvm-default", value, "x.kt"]).jvm_default,
                expected,
                "-jvm-default {value}"
            );
        }
        // The legacy `-Xjvm-default` spelling, with the mapping IntelliJ's own build applies
        // (build/compiler-options.bzl): `all` is today's `no-compatibility`, and `all-compatibility`
        // is today's `enable`. Reading `all` as "enable" would emit a `$DefaultImpls` class the
        // project deliberately does not have.
        for (value, expected) in [
            ("all", JvmDefaultMode::NoCompatibility),
            ("all-compatibility", JvmDefaultMode::Enable),
        ] {
            assert_eq!(
                parse_args(&[&format!("-Xjvm-default={value}"), "x.kt"]).jvm_default,
                expected,
                "-Xjvm-default={value}"
            );
        }
    }

    #[test]
    fn jvm_default_defaults_to_kotlincs_own_default() {
        use krusty::jvm::ir_emit::JvmDefaultMode;
        assert_eq!(
            parse_args(&["x.kt"]).jvm_default,
            JvmDefaultMode::Enable,
            "kotlinc 2.2+ defaults to `enable`"
        );
    }

    /// Every value kotlinc accepts selects a shape krusty emits; a value it does not accept is fatal
    /// rather than quietly compiled as the default, which would hand the build a different interface
    /// shape than it asked for.
    #[test]
    fn an_unknown_jvm_default_value_is_fatal_and_every_real_one_is_accepted() {
        use krusty::jvm::ir_emit::JvmDefaultMode;
        for (value, expected) in [
            ("disable", JvmDefaultMode::Disable),
            ("enable", JvmDefaultMode::Enable),
            ("no-compatibility", JvmDefaultMode::NoCompatibility),
        ] {
            let parsed = parse_args(&[&format!("-jvm-default={value}"), "x.kt"]);
            assert_eq!(parsed.jvm_default, expected, "-jvm-default={value}");
            assert!(parsed.errors.is_empty(), "{value}: {:?}", parsed.errors);
        }
        let parsed = parse_args(&["-jvm-default=sideways", "x.kt"]);
        assert!(
            parsed.errors.iter().any(|entry| entry.contains("sideways")),
            "an unknown value must fail the invocation: {:?}",
            parsed.errors
        );
    }

    /// kotlinc still reads an `-X…` value from the next argument, with a warning that the form is
    /// obsolete (measured on 2.4.20): `-Xjvm-default x.kt` takes `x.kt` as the mode.
    #[test]
    fn the_legacy_spelling_takes_the_next_argument_with_kotlincs_warning() {
        let parsed = parse_args(&["-Xjvm-default", "all", "x.kt"]);
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
        assert_eq!(
            parsed.jvm_default,
            JvmDefaultMode::parse_legacy("all").unwrap()
        );
        assert_eq!(
            parsed.argument_warnings,
            vec!["Advanced option value is passed in an obsolete form. Please use the '=' character to specify the value: -Xjvm-default=..."]
        );
    }

    #[test]
    fn an_unknown_jvm_default_value_is_reported_and_changes_nothing() {
        use krusty::jvm::ir_emit::JvmDefaultMode;
        let parsed = parse_args(&["-jvm-default=sideways", "x.kt"]);
        assert_eq!(parsed.jvm_default, JvmDefaultMode::Enable);
        assert!(
            parsed.errors.iter().any(|entry| entry.contains("sideways")),
            "an invalid value must be rejected, not silently accepted: {:?}",
            parsed.errors
        );
        assert_eq!(parsed.sources, vec!["x.kt".to_string()]);
    }

    #[test]
    fn kotlinc_style_flags() {
        let o = parse_args(&[
            "-d",
            "out.jar",
            "-cp",
            "a.jar:b/classes",
            "-module-name",
            "lib",
            "x.kt",
        ]);
        assert_eq!(o.dest, PathBuf::from("out.jar"));
        assert_eq!(
            o.classpath,
            vec![PathBuf::from("a.jar"), PathBuf::from("b/classes")]
        );
        assert_eq!(o.module_name, "lib");
        assert_eq!(o.sources, vec!["x.kt".to_string()]);
    }

    #[test]
    fn kotlinc_friend_paths_are_preserved_separately_from_the_classpath() {
        let o = parse_args(&[
            "-cp",
            "ordinary.jar:friend/classes",
            "-Xfriend-paths=friend/classes,second.jar",
            "x.kt",
        ]);
        // kotlinc splits friend paths on `,`, not on the path separator.
        assert_eq!(
            o.friend_paths,
            vec![PathBuf::from("friend/classes"), PathBuf::from("second.jar")]
        );
        assert_eq!(
            o.classpath,
            vec![
                PathBuf::from("ordinary.jar"),
                PathBuf::from("friend/classes")
            ]
        );
    }

    #[test]
    fn source_inputs_follow_shared_batch_capabilities() {
        let o = parse_args(&["main.kt", "script.kts", "Ignored.java"]);
        assert_eq!(
            o.sources,
            vec!["main.kt".to_string(), "Ignored.java".to_string()]
        );
        assert_eq!(
            o.ignored,
            vec!["script.kts (script compilation is not supported yet)".to_string()]
        );
    }

    /// One well-formed spelling of an argument: `-name` for a boolean, `-name=value` otherwise,
    /// with a legal value for a feature switch.
    fn well_formed(spec: &kotlinc_arguments::ArgumentSpec) -> String {
        match spec.kind {
            kotlinc_arguments::ValueKind::Bool => spec.name.clone(),
            _ if spec.name == "-XXLanguage" => "-XXLanguage:+ContextParameters".to_string(),
            _ => {
                let value = spec.legal_values().first().copied().unwrap_or("x");
                format!("{}={value}", spec.name)
            }
        }
    }

    /// Every current argument of every supported release takes the path its disposition names:
    /// an unsupported argument is refused and nothing else is reported, an inert one is accepted
    /// silently, and an applied one is never refused.
    #[test]
    fn every_kotlinc_argument_takes_its_dispositions_path() {
        for version in KotlinVersion::supported() {
            let catalog = Catalog::for_version(version).unwrap();
            for spec in catalog.arguments() {
                // kotlinc itself rejects an obsolete argument and only warns about a removed one.
                if spec.origin == Origin::Removed || spec.obsolete {
                    continue;
                }
                let argument = well_formed(spec);
                let reference = format!("-Xkotlin-reference-version={version}");
                let parsed = parse_args(&[&reference, &argument, "f.kt"]);
                let refusal =
                    format!("krusty does not implement the kotlinc argument '{argument}'");
                let context = format!("kotlinc {version} {argument}");
                match kotlinc_arguments::disposition::of(&spec.name) {
                    Some(Disposition::Unsupported) => {
                        assert_eq!(parsed.errors, vec![refusal], "{context}");
                        assert_eq!(parsed.argument_errors, Vec::<String>::new(), "{context}");
                        assert_eq!(parsed.ignored, Vec::<String>::new(), "{context}");
                    }
                    Some(Disposition::Inert) => {
                        assert_eq!(parsed.errors, Vec::<String>::new(), "{context}");
                        assert_eq!(parsed.argument_errors, Vec::<String>::new(), "{context}");
                        assert_eq!(parsed.ignored, Vec::<String>::new(), "{context}");
                    }
                    Some(Disposition::Applied) => {
                        assert!(!parsed.errors.contains(&refusal), "{context}");
                    }
                    None => panic!("{context} has no disposition"),
                }
            }
        }
    }

    /// A kotlinc argument krusty does not implement is an error; a `-X` flag kotlinc does not
    /// declare gets kotlinc's own warning; any other unknown option is kotlinc's error.
    #[test]
    fn accepts_standard_api_version_and_refuses_unimplemented_options() {
        let o = parse_args(&[
            "-include-runtime",
            "-api-version",
            "2.0",
            "-Xsomething",
            "f.kt",
        ]);
        assert_eq!(o.sources, vec!["f.kt".to_string()]);
        assert_eq!(o.language_settings.api_version, LanguageVersion::V2_0);
        assert_eq!(o.ignored, Vec::<String>::new());
        assert_eq!(
            o.errors,
            vec!["krusty does not implement the kotlinc argument '-include-runtime'"]
        );
        assert_eq!(
            o.argument_warnings,
            vec!["Flag is not supported by this version of the compiler: -Xsomething"]
        );
        assert_eq!(
            parse_args(&["-something", "f.kt"]).argument_errors,
            vec!["Invalid argument: -something"]
        );
    }

    /// Public `-language-version` selects source semantics. `-Xmetadata-version` remains an
    /// independent internal emission override on the same bounded 2.0–2.4 version domain.
    #[test]
    fn language_version_selects_semantics_and_the_stamp_override_stays_internal() {
        let current = parse_args(&["-language-version", "2.4", "f.kt"]);
        assert!(current.errors.is_empty(), "{:?}", current.errors);
        assert_eq!(
            current.language_settings.language_version,
            LanguageVersion::V2_4
        );
        assert_eq!(current.language_settings.api_version, LanguageVersion::V2_4);
        assert_eq!(current.metadata_version, None);
        assert_eq!(current.sources, vec!["f.kt".to_string()]);

        let older = parse_args(&["-language-version", "2.2", "f.kt"]);
        assert!(older.errors.is_empty(), "{:?}", older.errors);
        assert_eq!(
            older.language_settings.language_version,
            LanguageVersion::V2_2
        );
        assert_eq!(older.language_settings.api_version, LanguageVersion::V2_2);
        assert_eq!(older.metadata_version, None);
        assert!(older.language_settings.features.has("WhenGuards"));
        assert!(!older.language_settings.features.has("ContextParameters"));

        let explicitly_enabled = parse_args(&[
            "-XXLanguage:+ContextParameters",
            "-language-version",
            "2.2",
            "f.kt",
        ]);
        assert!(
            explicitly_enabled.errors.is_empty(),
            "{:?}",
            explicitly_enabled.errors
        );
        assert_eq!(
            explicitly_enabled.language_settings.language_version,
            LanguageVersion::V2_2
        );
        assert!(explicitly_enabled
            .language_settings
            .features
            .has("ContextParameters"));

        let older_api = parse_args(&["-language-version", "2.4", "-api-version", "2.2", "f.kt"]);
        assert!(older_api.errors.is_empty(), "{:?}", older_api.errors);
        assert_eq!(
            older_api.language_settings.api_version,
            LanguageVersion::V2_2
        );

        let newer_api = parse_args(&["-language-version", "2.2", "-api-version", "2.4", "f.kt"]);
        assert_eq!(
            newer_api.errors,
            ["-api-version (2.4) cannot be greater than -language-version (2.2).".to_owned()]
        );

        let stamp = parse_args(&["-Xmetadata-version", "2.2", "f.kt"]);
        assert_eq!(stamp.metadata_version, Some([2, 2, 0]));
        assert!(stamp.errors.is_empty(), "{:?}", stamp.errors);
        assert!(stamp.ignored.is_empty(), "{:?}", stamp.ignored);

        for experimental in [LanguageVersion::V2_5, LanguageVersion::V2_6] {
            let parsed = parse_args(&["-language-version", &experimental.to_string(), "f.kt"]);
            assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
            assert_eq!(parsed.language_settings.language_version, experimental);
            assert_eq!(parsed.language_settings.api_version, experimental);
            assert_eq!(parsed.warnings.len(), 1);
        }

        let supported = "2.0 (deprecated), 2.1 (deprecated), 2.2, 2.3, 2.4, 2.5 (experimental), 2.6 (experimental)";
        for bad in [
            "banana", "2", "2.4.0", "2.x", "-2.2", "+2.2", "0.0", "2.7", "999.1", "",
        ] {
            let language = parse_args(&["-language-version", bad, "f.kt"]);
            assert_eq!(
                language.errors,
                [format!(
                    "unknown language version: {bad}\nSupported language versions: {supported}"
                )],
                "{bad:?}"
            );
            let metadata = parse_args(&["-Xmetadata-version", bad, "f.kt"]);
            assert_eq!(metadata.metadata_version, None, "{bad:?}");
            assert_eq!(
                metadata.errors,
                [format!(
                    "unknown metadata version: {bad}\nSupported metadata versions: 2.0, 2.1, 2.2, 2.3, 2.4"
                )],
                "{bad:?}"
            );
        }

        assert_eq!(
            parse_args(&["-language-version"]).argument_errors,
            ["No value passed for argument -language-version".to_string()]
        );
        assert_eq!(
            parse_args(&["-Xmetadata-version"]).argument_errors,
            ["No value passed for argument -Xmetadata-version".to_string()]
        );

        let metadata = parse_args(&["-Xmetadata-version", "2.5", "f.kt"]);
        assert_eq!(
            metadata.errors,
            ["unknown metadata version: 2.5\nSupported metadata versions: 2.0, 2.1, 2.2, 2.3, 2.4".to_owned()]
        );
    }

    #[test]
    fn accepted_levels_and_no_option_default_follow_the_selected_kotlinc_release() {
        let no_option = parse_args(&["-Xkotlin-reference-version=2.4.10", "f.kt"]);
        assert!(no_option.errors.is_empty(), "{:?}", no_option.errors);
        assert_eq!(
            no_option.language_settings.language_version,
            LanguageVersion::V2_4
        );
        assert_eq!(
            no_option.language_settings.api_version,
            LanguageVersion::V2_4
        );

        let older_release = parse_args(&[
            "-Xkotlin-reference-version=2.4.10",
            "-language-version",
            "2.6",
            "f.kt",
        ]);
        assert_eq!(
            older_release.errors,
            ["unknown language version: 2.6\nSupported language versions: 2.0 (deprecated), 2.1 (deprecated), 2.2, 2.3, 2.4, 2.5 (experimental)".to_owned()]
        );

        let current_release = parse_args(&[
            "-Xkotlin-reference-version=2.4.20",
            "-language-version",
            "2.6",
            "f.kt",
        ]);
        assert!(
            current_release.errors.is_empty(),
            "{:?}",
            current_release.errors
        );
        assert_eq!(
            current_release.language_settings.language_version,
            LanguageVersion::V2_6
        );
    }

    #[test]
    fn version_status_warnings_match_kotlinc() {
        assert_eq!(
            parse_args(&["-language-version", "2.0", "f.kt"]).warnings,
            [CliWarning {
                name: Some(WarningName::DeprecatedLanguageVersion),
                message: "language version 2.0 is deprecated and its support will be removed in a future version of Kotlin. Update the version to 2.2.".to_owned(),
            }]
        );
        assert_eq!(
            parse_args(&[
                "-language-version",
                "2.5",
                "-api-version",
                "2.0",
                "f.kt",
            ])
            .warnings,
            [
                CliWarning {
                    name: Some(WarningName::DeprecatedLanguageVersion),
                    message: "API version 2.0 is deprecated and its support will be removed in a future version of Kotlin. Update the version to 2.2.".to_owned(),
                },
                CliWarning {
                    name: Some(WarningName::ExperimentalLanguageVersion),
                    message: "language version 2.5 is experimental, there are no backwards compatibility guarantees for new language and library features. Use the stable version 2.4 instead.".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn suppress_version_warnings_drops_only_version_status_warnings() {
        let suppressed = parse_args(&[
            "-language-version",
            "2.0",
            "-api-version",
            "2.0",
            "-Xsuppress-version-warnings",
            "f.kt",
        ]);
        assert!(suppressed.errors.is_empty(), "{:?}", suppressed.errors);
        assert!(suppressed.ignored.is_empty(), "{:?}", suppressed.ignored);
        assert!(suppressed.warnings.is_empty(), "{:?}", suppressed.warnings);

        let experimental = parse_args(&[
            "-language-version",
            "2.6",
            "-Xsuppress-version-warnings",
            "-Xkotlin-reference-version=2.4.20",
            "f.kt",
        ]);
        assert!(
            experimental.warnings.is_empty(),
            "{:?}",
            experimental.warnings
        );

        let redundant = parse_args(&[
            "-language-version",
            "2.4",
            "-Xsuppress-version-warnings",
            "-Xcontext-parameters",
            "f.kt",
        ]);
        assert_eq!(
            redundant.warnings,
            [CliWarning {
                name: Some(WarningName::RedundantCliArg),
                message: "the argument '-Xcontext-parameters' is redundant for the current language version 2.4.".to_string(),
            }]
        );
    }

    /// Warning policy is keyed by stable diagnostic identity. Unknown and repeated names are
    /// rejected instead of being retained as inert strings that the compiler cannot honor.
    #[test]
    fn warning_level_configures_named_diagnostics() {
        for (severity, expected) in [
            ("error", WarningLevel::Error),
            ("warning", WarningLevel::Warning),
            ("disabled", WarningLevel::Disabled),
        ] {
            let flag = format!("-Xwarning-level=REDUNDANT_CLI_ARG:{severity}");
            let parsed = parse_args(&[&flag, "f.kt"]);
            assert!(parsed.errors.is_empty(), "{flag}: {:?}", parsed.errors);
            assert!(parsed.ignored.is_empty(), "{flag}: {:?}", parsed.ignored);
            assert_eq!(
                parsed.warning_policy.level(WarningName::RedundantCliArg),
                expected,
                "{flag}"
            );
            assert_eq!(parsed.sources, vec!["f.kt".to_string()]);
        }

        let parsed = parse_args(&[
            "-Xwarning-level=NO_SUCH_DIAGNOSTIC:error",
            "-Xwarning-level=REDUNDANT_CLI_ARG:disabled",
            "f.kt",
        ]);
        assert_eq!(
            parsed.errors,
            ["warning with name \"NO_SUCH_DIAGNOSTIC\" does not exist".to_string()]
        );
        assert_eq!(
            parsed.warning_policy.level(WarningName::RedundantCliArg),
            WarningLevel::Disabled
        );

        for (bad, severity) in [
            ("-Xwarning-level=REDUNDANT_CLI_ARG:loud", "loud"),
            // kotlinc's spellings are case-sensitive.
            ("-Xwarning-level=REDUNDANT_CLI_ARG:DISABLED", "DISABLED"),
            ("-Xwarning-level=REDUNDANT_CLI_ARG:Warning", "Warning"),
            ("-Xwarning-level=REDUNDANT_CLI_ARG:", ""),
        ] {
            let parsed = parse_args(&[bad, "f.kt"]);
            assert_eq!(
                parsed.errors,
                [format!(
                    "invalid severity '{severity}' in -Xwarning-level=REDUNDANT_CLI_ARG:{severity}; \
                     supported severities: error, warning, disabled"
                )],
                "{bad}"
            );
            assert!(parsed.ignored.is_empty(), "{bad}: {:?}", parsed.ignored);
        }

        let parsed = parse_args(&["-Xwarning-level=REDUNDANT_CLI_ARG", "f.kt"]);
        assert_eq!(
            parsed.errors,
            [
                "invalid value 'REDUNDANT_CLI_ARG' for -Xwarning-level: expected \
             <NAME>:<error|warning|disabled>"
                    .to_string()
            ]
        );
        let parsed = parse_args(&["-Xwarning-level=:disabled", "f.kt"]);
        assert_eq!(
            parsed.errors,
            ["warning with name \"\" does not exist".to_string()]
        );

        let parsed = parse_args(&[
            "-Xwarning-level=REDUNDANT_CLI_ARG:error",
            "-Xwarning-level=REDUNDANT_CLI_ARG:disabled",
            "f.kt",
        ]);
        assert_eq!(
            parsed.errors,
            ["warning with name \"REDUNDANT_CLI_ARG\" has already been configured".to_string()]
        );
        assert_eq!(
            parsed.warning_policy.level(WarningName::RedundantCliArg),
            WarningLevel::Error,
            "a rejected duplicate must not mutate the first configuration"
        );
    }

    #[test]
    fn redundant_language_feature_argument_has_a_named_warning() {
        let parsed = parse_args(&["-language-version", "2.4", "-Xcontext-parameters", "f.kt"]);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(
            parsed.warnings,
            [CliWarning {
                name: Some(WarningName::RedundantCliArg),
                message: "the argument '-Xcontext-parameters' is redundant for the current language version 2.4.".to_string(),
            }]
        );
    }

    #[test]
    fn nested_type_aliases_flag_is_redundant_on_2_4() {
        let parsed = parse_args(&["-language-version", "2.4", "-Xnested-type-aliases", "f.kt"]);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert_eq!(
            parsed.warnings,
            [CliWarning {
                name: Some(WarningName::RedundantCliArg),
                message: "the argument '-Xnested-type-aliases' is redundant for the current language version 2.4.".to_string(),
            }]
        );
    }

    /// `-Xexplicit-api` selects diagnostics rather than a language feature: no mode is redundant,
    /// the last one wins, and an unknown mode is kotlinc's error.
    #[test]
    fn explicit_api_mode_selects_its_checks() {
        let features = |arguments: &[&str]| {
            let parsed = parse_args(arguments);
            assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
            assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
            assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
            let features = parsed.language_settings.features;
            (
                features.has("ExplicitApiStrict"),
                features.has("ExplicitApiWarning"),
            )
        };
        assert_eq!(features(&["-Xexplicit-api=strict", "f.kt"]), (true, false));
        assert_eq!(features(&["-Xexplicit-api=warning", "f.kt"]), (false, true));
        assert_eq!(
            features(&["-Xexplicit-api=disable", "f.kt"]),
            (false, false)
        );
        assert_eq!(
            features(&["-Xexplicit-api=strict", "-Xexplicit-api=disable", "f.kt"]),
            (false, false)
        );
        assert_eq!(
            parse_args(&["-Xexplicit-api=bogus", "f.kt"]).errors,
            [
                "unknown value for parameter -Xexplicit-api: 'bogus'. Value should be one of \
              {disable, strict, warning}"
            ]
        );
    }

    /// `-opt-in` accepts markers in both spellings, repeated and comma-separated.
    #[test]
    fn opt_in_accepts_every_spelling() {
        let parsed = parse_args(&[
            "-opt-in=a.B,c.D",
            "-opt-in",
            "e.F",
            "-opt-in=kotlin.contracts.ExperimentalContracts",
            "f.kt",
        ]);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(
            parsed
                .language_settings
                .features
                .opted_in()
                .collect::<Vec<_>>(),
            [
                "a.B",
                "c.D",
                "e.F",
                "kotlin.contracts.ExperimentalContracts"
            ]
        );
    }

    /// kotlinc warns about any `-X` flag its JVM compiler does not declare (`-Xwasm-kclass-fqn`
    /// belongs to the Wasm compiler) and compiles as if it were absent. The flag is therefore not
    /// `ignored` (the worker refuses requests with ignored options) nor an error.
    #[test]
    fn an_undeclared_advanced_flag_is_warned_not_ignored() {
        let parsed = parse_args(&["-Xwasm-kclass-fqn", "f.kt"]);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert!(
            parsed.argument_errors.is_empty(),
            "{:?}",
            parsed.argument_errors
        );
        assert_eq!(
            parsed.argument_warnings,
            vec!["Flag is not supported by this version of the compiler: -Xwasm-kclass-fqn"]
        );
        assert_eq!(parsed.sources, vec!["f.kt".to_string()]);
    }

    #[test]
    fn jvm_target_sets_class_major_version() {
        assert_eq!(jvm_target_to_major("1.8"), Some(52));
        assert_eq!(jvm_target_to_major("8"), Some(52));
        assert_eq!(jvm_target_to_major("9"), Some(53));
        assert_eq!(jvm_target_to_major("21"), Some(65));
        assert_eq!(jvm_target_to_major("25"), Some(69));
        assert_eq!(jvm_target_to_major("banana"), None);

        // The parsed option carries the mapped major; an unknown value is reported, not applied.
        let o = parse_args(&["-jvm-target", "25", "f.kt"]);
        assert_eq!(o.jvm_target_major, Some(69));
        assert_eq!(o.sources, vec!["f.kt".to_string()]);

        let bad = parse_args(&["-jvm-target", "banana", "f.kt"]);
        assert_eq!(bad.jvm_target_major, None);
        assert!(bad.ignored.contains(&"-jvm-target banana".to_string()));
        assert_eq!(bad.sources, vec!["f.kt".to_string()]);
    }

    #[test]
    fn java_parameters_is_an_active_backend_option() {
        let options = parse_args(&["-java-parameters", "f.kt"]);
        assert!(options.java_parameters);
        assert!(options.ignored.is_empty(), "{:?}", options.ignored);
        assert_eq!(options.sources, vec!["f.kt".to_string()]);
    }

    #[test]
    fn jdk_home_and_no_jdk_flags() {
        let o = parse_args(&["-jdk-home", "/opt/jdk", "f.kt"]);
        assert_eq!(o.jdk_home, Some(PathBuf::from("/opt/jdk")));
        assert!(!o.no_jdk);
        assert_eq!(o.sources, vec!["f.kt".to_string()]); // value consumed, not a source
        let o = parse_args(&["-no-jdk", "f.kt"]);
        assert!(o.no_jdk);
        // `-no-jdk` suppresses the JDK even with a `-jdk-home`; effective cp adds nothing.
        let o = parse_args(&["-no-stdlib", "-no-jdk", "-jdk-home", "/opt/jdk", "f.kt"]);
        assert_eq!(o.effective_classpath().unwrap(), o.classpath);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krusty_cp_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn empty_jar(path: &std::path::Path) {
        // The smallest valid zip: an end-of-central-directory record and nothing else.
        std::fs::write(path, b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    }

    #[test]
    fn a_missing_classpath_entry_is_a_warning_in_kotlinc_words() {
        let problems = classpath_entry_problems(&[PathBuf::from("/definitely/not/there.jar")]);
        assert_eq!(
            problems,
            vec![ClasspathEntryProblem::Missing(PathBuf::from(
                "/definitely/not/there.jar"
            ))]
        );
        assert_eq!(
            problems[0].to_string(),
            "warning: classpath entry points to a non-existent location: /definitely/not/there.jar"
        );
    }

    #[test]
    fn a_jar_that_cannot_be_opened_is_a_warning() {
        let dir = scratch("truncated");
        let jar = dir.join("core.jar");
        std::fs::write(&jar, b"PK\x03\x04 not really a zip").unwrap();
        let problems = classpath_entry_problems(std::slice::from_ref(&jar));
        assert_eq!(
            problems,
            vec![ClasspathEntryProblem::Unreadable {
                path: jar.clone(),
                reason: "not a readable archive (invalid Zip archive: Could not find EOCD)"
                    .to_string(),
            }]
        );
        // kotlinc warns (`WARN: Error while reading zip file`) and continues; so does krusty.
        assert_eq!(
            problems[0].to_string(),
            format!(
                "warning: cannot read classpath entry {}: not a readable archive \
                 (invalid Zip archive: Could not find EOCD)",
                jar.display()
            )
        );
    }

    #[test]
    fn readable_entries_of_every_kind_raise_nothing() {
        let dir = scratch("fine");
        let jar = dir.join("ok.jar");
        empty_jar(&jar);
        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        // A jimage or any non-archive file is read by the classpath reader itself; only that it
        // opens is checked here.
        let modules = dir.join("modules");
        std::fs::write(&modules, b"JIMAGE").unwrap();
        assert_eq!(
            classpath_entry_problems(&[jar, classes, modules]),
            Vec::<ClasspathEntryProblem>::new()
        );
    }

    #[test]
    fn effective_classpath_ignores_a_missing_jdk_home() {
        // A non-existent `-jdk-home` contributes nothing (a bad env must not break an explicit cp).
        let o = parse_args(&[
            "-no-stdlib",
            "-jdk-home",
            "/definitely/not/a/jdk",
            "-cp",
            "a.jar",
            "f.kt",
        ]);
        assert_eq!(
            o.effective_classpath().unwrap(),
            vec![PathBuf::from("a.jar")]
        );
    }

    #[test]
    fn effective_classpath_uses_the_shared_jvm_input_inventory_for_the_jdk() {
        let root = scratch("jdk_inventory");
        std::fs::create_dir_all(root.join("lib")).unwrap();
        let modules = root.join("lib/modules");
        std::fs::write(&modules, b"JIMAGE").unwrap();

        let options = parse_args(&[
            "-no-stdlib",
            "-jdk-home",
            root.to_str().expect("UTF-8 scratch path"),
            "f.kt",
        ]);
        let actual = options.effective_classpath().unwrap();
        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], Some(&root));
        assert_eq!(actual, vec![modules]);
        assert_eq!(
            actual,
            inventory.effective_classpath(),
            "the CLI must pass exactly the inventory's selected classpath roots"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A JDK 8 home has no `lib/modules`; its bootclasspath is `jre/lib/rt.jar`. The CLI must
    /// still route through the shared inventory so its selection and the provider's agree.
    #[test]
    fn effective_classpath_falls_back_to_a_jdk8_rt_jar() {
        let root = scratch("jdk8_inventory");
        std::fs::create_dir_all(root.join("jre/lib")).unwrap();
        let rt_jar = root.join("jre/lib/rt.jar");
        empty_jar(&rt_jar);

        let options = parse_args(&[
            "-no-stdlib",
            "-jdk-home",
            root.to_str().expect("UTF-8 scratch path"),
            "f.kt",
        ]);
        let actual = options.effective_classpath().unwrap();
        let inventory =
            JvmCompilationInputInventory::from_explicit_classpath_and_jdk(&[], Some(&root));
        assert_eq!(actual, vec![rt_jar]);
        assert_eq!(
            actual,
            inventory.effective_classpath(),
            "the CLI must pass exactly the inventory's selected classpath roots"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn effective_classpath_adds_stdlib_unless_disabled() {
        let with_stdlib = parse_args(&["-no-reflect", "-no-jdk", "f.kt"])
            .effective_classpath()
            .expect("test toolchain must provide stdlib");
        let stdlib = krusty::jvm::kotlin_stdlib_jar().expect("test toolchain must provide stdlib");
        assert!(with_stdlib.contains(&stdlib));
        assert!(parse_args(&["-no-stdlib", "-no-jdk", "f.kt"])
            .effective_classpath()
            .unwrap()
            .is_empty());
    }

    /// Plugin switches are read in kotlinc's syntax, never reported as ignored, and `-P` consumes its
    /// value rather than leaving it to be read as a source file.
    #[test]
    fn plugin_switches_are_read_not_ignored() {
        let parsed = parse_args(&[
            "-Xplugin=/k/a.jar,/k/b.jar",
            "-P",
            "plugin:org.example.widget:annotation=p.Open",
            "-Xplugin=/k/c.jar",
            "f.kt",
        ]);
        assert_eq!(parsed.ignored, Vec::<String>::new());
        assert_eq!(parsed.errors, Vec::<String>::new());
        assert_eq!(parsed.sources, vec!["f.kt".to_string()]);
        assert_eq!(
            parsed.plugins.plugin_jars,
            vec!["/k/a.jar", "/k/b.jar", "/k/c.jar"]
        );
        assert_eq!(
            parsed.plugins.options,
            vec![krusty::plugins::cli::PluginOption {
                id: "org.example.widget".to_string(),
                key: "annotation".to_string(),
                value: "p.Open".to_string(),
            }]
        );
    }

    /// A plugin switch kotlinc rejects fails the invocation instead of compiling without the plugin.
    #[test]
    fn malformed_plugin_switches_are_errors() {
        let parsed = parse_args(&["-P", "annotation=p.Open", "f.kt"]);
        assert_eq!(
            parsed.errors,
            vec!["wrong plugin option format: annotation=p.Open, should be \
                 plugin:<pluginId>:<optionName>=<value>"
                .to_string()]
        );
        assert_eq!(parsed.sources, vec!["f.kt".to_string()]);

        let parsed = parse_args(&["-Xplugin=/k/a.jar", "-Xcompiler-plugin=/k/b.jar", "f.kt"]);
        assert_eq!(
            parsed.errors,
            vec![
                "mixing legacy and modern plugin arguments is prohibited. Please use only one \
                 syntax"
                    .to_string()
            ]
        );
    }

    /// A jar named `name` declaring `registrars` in `service` (a `META-INF/services` file name).
    fn jar_declaring(
        dir: &std::path::Path,
        name: &str,
        service: &str,
        registrars: &[&str],
    ) -> String {
        use std::io::Write;
        let jar = dir.join(name);
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        archive
            .start_file(
                format!("META-INF/services/{service}"),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(registrars.join("\n").as_bytes()).unwrap();
        archive.finish().unwrap();
        jar.display().to_string()
    }

    const REGISTRAR_SERVICE: &str = "org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar";

    /// A jar declaring the registrar of the builtin extension `plugin_id`, as the registry records it.
    fn builtin_plugin_jar(dir: &std::path::Path, name: &str, plugin_id: &str) -> String {
        let registry = PluginRegistry::with_builtins();
        let extension = registry
            .extensions()
            .iter()
            .find(|extension| extension.plugin_id == plugin_id)
            .unwrap();
        jar_declaring(dir, name, REGISTRAR_SERVICE, &[extension.registrar])
    }

    #[test]
    fn no_plugin_requested_runs_no_native_pass() {
        let resolution = parse_args(&["f.kt"]).resolve_plugins(&[]);
        assert!(resolution.native.is_empty());
        assert_eq!(resolution.notes, Vec::<String>::new());
        assert_eq!(resolution.errors, Vec::<String>::new());
    }

    #[test]
    fn the_serialization_plugin_resolves_to_the_native_pass_with_a_note() {
        let dir = scratch("serialization_plugin");
        let jar = builtin_plugin_jar(
            &dir,
            "serialization-plugin.jar",
            krusty::plugins::cli::SERIALIZATION_PLUGIN_ID,
        );
        let resolution = parse_args(&[&format!("-Xplugin={jar}"), "f.kt"]).resolve_plugins(&[]);
        assert_eq!(
            resolution.native.plugin_ids(),
            vec![krusty::plugins::cli::SERIALIZATION_PLUGIN_ID]
        );
        assert_eq!(
            resolution.notes,
            vec![PluginDiagnostic::NativeSubstitution {
                plugin_id: krusty::plugins::cli::SERIALIZATION_PLUGIN_ID.to_string(),
                jar: Some(jar),
            }
            .message()]
        );
        assert_eq!(resolution.errors, Vec::<String>::new());
    }

    /// The command line runs no codegen host: a KSP request would otherwise be reported "hosted"
    /// while its generated sources silently never appear.
    #[test]
    fn a_codegen_host_plugin_is_an_error_on_the_command_line() {
        let dir = scratch("ksp_plugin");
        let jar = builtin_plugin_jar(&dir, "ksp.jar", krusty::plugins::cli::KSP_PLUGIN_ID);
        let resolution = parse_args(&[&format!("-Xplugin={jar}"), "f.kt"]).resolve_plugins(&[]);
        assert!(resolution.native.is_empty());
        assert_eq!(resolution.notes, Vec::<String>::new());
        assert_eq!(
            resolution.errors,
            vec![PluginDiagnostic::HostUnavailable {
                plugin_id: krusty::plugins::cli::KSP_PLUGIN_ID.to_string(),
            }
            .message()]
        );
    }

    #[test]
    fn an_unknown_plugin_and_a_missing_jar_are_errors() {
        let dir = scratch("unknown_plugin");
        let jar = jar_declaring(
            &dir,
            "widget-plugin.jar",
            REGISTRAR_SERVICE,
            &["org.example.widget.WidgetRegistrar"],
        );
        let resolution = parse_args(&[&format!("-Xplugin={jar}"), "f.kt"]).resolve_plugins(&[]);
        assert_eq!(
            resolution.errors,
            vec![PluginDiagnostic::Unsupported { plugin: jar }.message()]
        );

        let missing = dir.join("absent-plugin.jar").display().to_string();
        let resolution =
            parse_args(&[&format!("-Xcompiler-plugin={missing}"), "f.kt"]).resolve_plugins(&[]);
        assert_eq!(
            resolution.errors,
            vec![format!("no plugins found in given classpath: {missing}")]
        );
    }

    #[test]
    fn version_and_help() {
        assert!(parse_args(&["-version"]).print_version);
        assert!(parse_args(&["-help"]).print_help);
    }

    #[test]
    fn default_module_name_is_main() {
        assert_eq!(parse_args(&["f.kt"]).module_name, "main");
    }
}
