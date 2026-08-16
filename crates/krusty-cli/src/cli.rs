//! kotlinc-compatible command-line parsing, so `krusty` can stand in for `kotlinc` in a build:
//! same common flags (`-d`, `-classpath`/`-cp`, `-include-runtime`, `-module-name`, `-jvm-target`,
//! `-version`, `-help`, …), source files **or directories**, `@argfile`s, and graceful handling of
//! options krusty doesn't implement (ignored with a note, rather than treated as source files).
//! An explicit option that selects an output shape krusty cannot emit is instead a fatal error: it
//! must never compile successfully under a different shape.

use std::path::PathBuf;

use krusty::features::LangFeatures;
use krusty::jvm::classpath::platform_jdk_modules;
use krusty::jvm::ir_emit::{JvmDefaultMode, LambdaMode, LambdaModes};

pub struct Options {
    /// Output directory or `.jar` (kotlinc `-d`).
    pub dest: PathBuf,
    /// Classpath entries (dirs/jars).
    pub classpath: Vec<PathBuf>,
    /// `.kt` source files (directories already expanded).
    pub sources: Vec<String>,
    /// Module name → `<module>.kotlin_module` (kotlinc `-module-name`, default `main`).
    pub module_name: String,
    /// Language features enabled via `-XXLanguage:+Foo` / `-X<feature>` (drop-in `kotlinc` flags).
    pub features: LangFeatures,
    /// Options accepted for compatibility but not acted on (reported once).
    pub ignored: Vec<String>,
    /// Invalid or explicitly requested-but-unemittable options. The driver reports these and exits
    /// before compilation rather than silently producing a different artifact.
    pub errors: Vec<String>,
    /// `-version` / `-help` requested (handled before compiling).
    pub print_version: bool,
    pub print_help: bool,
    /// `-jdk-home <dir>`: the JDK whose `lib/modules` (java.base etc.) seeds the bootclasspath.
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
}

impl Default for Options {
    fn default() -> Options {
        Options {
            dest: PathBuf::from("krusty-out"),
            classpath: Vec::new(),
            sources: Vec::new(),
            module_name: "main".to_string(),
            features: LangFeatures::new(),
            ignored: Vec::new(),
            errors: Vec::new(),
            print_version: false,
            print_help: false,
            jdk_home: None,
            no_stdlib: false,
            no_reflect: false,
            no_jdk: false,
            jvm_target_major: None,
            jvm_default: JvmDefaultMode::default(),
            lambda_modes: LambdaModes::default(),
            no_param_assertions: false,
            no_call_assertions: false,
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

/// kotlinc flags that take a following value but which krusty ignores (accept + drop the value).
const IGNORED_WITH_VALUE: &[&str] = &[
    "-language-version",
    "-api-version",
    "-kotlin-home",
    "-Xexplicit-api",
    "-opt-in",
    "-P",
    "-script-templates",
    "-expression",
    "-e",
];
/// kotlinc valueless flags that krusty ignores (accept + drop).
const IGNORED_FLAGS: &[&str] = &[
    "-include-runtime",
    "-nowarn",
    "-verbose",
    "-Werror",
    "-progressive",
    "-script",
    "-java-parameters",
    "-Xuse-ir",
];

/// Record a `-jvm-default`/`-Xjvm-default` value, reporting one krusty does not model instead of
/// silently compiling under a different interface shape than the build asked for.
fn apply_jvm_default(
    opts: &mut Options,
    flag: &str,
    value: &str,
    parse_value: fn(&str) -> Option<JvmDefaultMode>,
) {
    // An unknown value is fatal. Merely recording it as an ignored option would let the driver
    // continue with `Enable`, silently emitting a different interface shape than the invocation
    // requested. Every value kotlinc accepts is emitted, so there is no second rejection class.
    match parse_value(value) {
        Some(mode) => opts.jvm_default = mode,
        None => opts
            .errors
            .push(format!("invalid value '{value}' for {flag}")),
    }
}

/// Split a classpath string on the platform separator (`:` on Unix).
fn split_classpath(v: &str) -> Vec<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    v.split(sep)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn collect_sources(path: &str, out: &mut Vec<String>, ignored: &mut Vec<String>) {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                collect_sources(&e.to_string_lossy(), out, ignored);
            }
        }
    } else if krusty::source::is_batch_compilable_path(p) {
        out.push(path.to_string());
    } else if krusty::source::kind(p).is_some() {
        ignored.push(format!("{path} (script compilation is not supported yet)"));
    } else if path.ends_with(".java") {
        ignored.push(format!("{path} (no Java source front end yet)"));
    }
}

/// Parse argv (already skipping the program name). `@file` argfiles are expanded inline.
pub fn parse(argv: impl IntoIterator<Item = String>) -> Options {
    let mut opts = Options::default();
    let mut raw: Vec<String> = Vec::new();
    for a in argv {
        if let Some(file) = a.strip_prefix('@') {
            if let Ok(contents) = std::fs::read_to_string(file) {
                raw.extend(contents.split_whitespace().map(|s| s.to_string()));
                continue;
            }
        }
        raw.push(a);
    }

    let mut it = raw.into_iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-d" => opts.dest = PathBuf::from(it.next().unwrap_or_else(|| ".".into())),
            "-cp" | "-classpath" | "-class-path" => {
                if let Some(v) = it.next() {
                    opts.classpath.extend(split_classpath(&v));
                }
            }
            "-module-name" => {
                if let Some(v) = it.next() {
                    opts.module_name = v;
                }
            }
            "-jdk-home" => {
                if let Some(v) = it.next() {
                    opts.jdk_home = Some(PathBuf::from(v));
                }
            }
            "-Xno-param-assertions" => opts.no_param_assertions = true,
            "-Xno-call-assertions" => opts.no_call_assertions = true,
            // `-Xlambdas` / `-Xsam-conversions` select how a lambda and a SAM conversion are
            // realized: `indy` is a `LambdaMetafactory` call site (kotlinc's default since 2.0),
            // `class` gives each lambda its own class extending `kotlin.jvm.internal.Lambda`. The
            // two differ in the emitted class SET, so each value selects its own emitter strategy
            // rather than being advisory.
            flag if flag.starts_with("-Xlambdas=") || flag.starts_with("-Xsam-conversions=") => {
                let (name, value) = flag
                    .split_once('=')
                    .expect("the guard above already matched an `=`");
                match value {
                    "indy" | "class" => {
                        let mode = if value == "indy" {
                            LambdaMode::Indy
                        } else {
                            LambdaMode::Class
                        };
                        if name == "-Xlambdas" {
                            opts.lambda_modes.lambdas = mode;
                        } else {
                            opts.lambda_modes.sam_conversions = mode;
                        }
                    }
                    _ => opts.errors.push(format!(
                        "{name}={value} selects an output shape krusty does not emit"
                    )),
                }
            }
            "-no-stdlib" => opts.no_stdlib = true,
            "-no-reflect" => opts.no_reflect = true,
            "-no-jdk" => opts.no_jdk = true,
            "-jvm-target" => {
                // Honor the target: it sets the emitted class-file version. An unrecognized value is
                // reported like any other ignored option rather than silently defaulting.
                match it.next() {
                    Some(v) => match jvm_target_to_major(&v) {
                        Some(major) => opts.jvm_target_major = Some(major),
                        None => opts.ignored.push(format!("-jvm-target {v}")),
                    },
                    None => opts.ignored.push("-jvm-target".to_string()),
                }
            }
            // `-jvm-default` decides the JVM shape of an interface's members with bodies. It takes
            // both the `flag value` and `flag=value` forms; the legacy `-Xjvm-default` spelling names
            // the same three shapes differently and, like every kotlinc `-X…` flag, takes its value
            // with `=` only — accepting a space form there would swallow the next source file.
            "-jvm-default" => match it.next() {
                Some(v) => apply_jvm_default(&mut opts, "-jvm-default", &v, JvmDefaultMode::parse),
                None => opts
                    .errors
                    .push("missing value for -jvm-default".to_string()),
            },
            flag if flag.starts_with("-jvm-default=") => {
                let value = flag
                    .strip_prefix("-jvm-default=")
                    .unwrap_or_default()
                    .to_string();
                apply_jvm_default(&mut opts, "-jvm-default", &value, JvmDefaultMode::parse);
            }
            flag if flag.starts_with("-Xjvm-default=") => {
                let value = flag
                    .strip_prefix("-Xjvm-default=")
                    .unwrap_or_default()
                    .to_string();
                apply_jvm_default(
                    &mut opts,
                    "-Xjvm-default",
                    &value,
                    JvmDefaultMode::parse_legacy,
                );
            }
            "-version" => opts.print_version = true,
            "-help" | "-h" | "-X" => opts.print_help = true,
            flag if IGNORED_WITH_VALUE.contains(&flag) => {
                let _ = it.next(); // consume + drop the value
                opts.ignored.push(flag.to_string());
            }
            flag if IGNORED_FLAGS.contains(&flag) => opts.ignored.push(flag.to_string()),
            // Language-feature flags (`-XXLanguage:+Foo,-Bar`, `-Xname-based-destructuring=…`) — a
            // drop-in honors the same toggles kotlinc does so flag-gated syntax compiles.
            flag if opts.features.apply_cli_arg(flag) => {}
            // Unknown option: ignore it (don't mistake it for a source file). kotlinc's `-X...` and
            // `-P...` advanced flags land here.
            flag if flag.starts_with('-') => opts.ignored.push(flag.to_string()),
            // A positional argument: a source file or directory.
            other => collect_sources(other, &mut opts.sources, &mut opts.ignored),
        }
    }
    opts
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
            if let Some(modules) = platform_jdk_modules(self.jdk_home.as_deref()) {
                cp.push(modules);
            }
        }
        Ok(cp)
    }
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
  -module-name <name>   name of the generated <name>.kotlin_module (default: main)
  -include-runtime      accepted (no-op: krusty does not bundle the stdlib)
  -jvm-target <v>        class-file version to emit (1.8→v52, 9→v53, …, 25→v69; default v52)
  -version              print version and exit
  -jvm-default <mode>   interface default-method strategy: enable | no-compatibility
                        (legacy -Xjvm-default=all | all-compatibility)
  -Xno-param-assertions omit JVM entry guards for non-null parameters
  -Xno-call-assertions  omit JVM assertions on platform-typed call results
  -Xlambdas=indy        emit lambdas through LambdaMetafactory (class is not supported)
  -Xsam-conversions=indy emit SAM conversions through LambdaMetafactory
  -help                 print this help and exit

Sources may be .kt files or directories (scanned recursively). Kotlin scripts are not yet compiled.
Unsupported compatibility options are ignored with a note. Invalid values and options that request
an unemittable output shape are errors; krusty never substitutes a different artifact shape.";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Options {
        parse(args.iter().map(|s| s.to_string()))
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

    /// kotlinc's `-X…` flags take their value with `=` only. Consuming a following argument would
    /// eat the source file that comes after the flag.
    #[test]
    fn the_legacy_spelling_never_consumes_the_next_argument() {
        let parsed = parse_args(&["-Xjvm-default", "x.kt"]);
        assert_eq!(
            parsed.sources,
            vec!["x.kt".to_string()],
            "the source file must survive: {parsed:?}",
            parsed = parsed.ignored
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
    fn source_inputs_follow_shared_batch_capabilities() {
        let o = parse_args(&["main.kt", "script.kts", "Ignored.java"]);
        assert_eq!(o.sources, vec!["main.kt".to_string()]);
        assert_eq!(
            o.ignored,
            vec![
                "script.kts (script compilation is not supported yet)".to_string(),
                "Ignored.java (no Java source front end yet)".to_string(),
            ]
        );
    }

    #[test]
    fn ignores_unsupported_with_and_without_value() {
        let o = parse_args(&[
            "-include-runtime",
            "-language-version",
            "2.0",
            "-Xsomething",
            "f.kt",
        ]);
        // -language-version consumed its value (2.0), not treated as a source.
        assert_eq!(o.sources, vec!["f.kt".to_string()]);
        assert!(o.ignored.contains(&"-include-runtime".to_string()));
        assert!(o.ignored.contains(&"-language-version".to_string()));
        assert!(o.ignored.contains(&"-Xsomething".to_string()));
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
