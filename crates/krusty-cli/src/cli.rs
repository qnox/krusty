//! kotlinc-compatible command-line parsing, so `krusty` can stand in for `kotlinc` in a build:
//! same common flags (`-d`, `-classpath`/`-cp`, `-include-runtime`, `-module-name`, `-jvm-target`,
//! `-version`, `-help`, …), source files **or directories**, `@argfile`s, and graceful handling of
//! options krusty doesn't implement (ignored with a note, rather than treated as source files).

use std::path::PathBuf;

use krusty::features::LangFeatures;
use krusty::jvm::classpath::platform_jdk_modules;

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
    /// `-version` / `-help` requested (handled before compiling).
    pub print_version: bool,
    pub print_help: bool,
    /// `-jdk-home <dir>`: the JDK whose `lib/modules` (java.base etc.) seeds the bootclasspath.
    pub jdk_home: Option<PathBuf>,
    /// `-no-jdk`: do NOT add the platform JDK to the classpath (kotlinc semantics).
    pub no_jdk: bool,
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
            print_version: false,
            print_help: false,
            jdk_home: None,
            no_jdk: false,
            jvm_target_major: None,
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
    "-jvm-default",
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
    "-no-stdlib",
    "-no-reflect",
    "-nowarn",
    "-verbose",
    "-Werror",
    "-progressive",
    "-script",
    "-java-parameters",
    "-Xjvm-default",
    "-Xuse-ir",
];

/// Split a classpath string on the platform separator (`:` on Unix).
fn split_classpath(v: &str) -> Vec<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    v.split(sep)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Recursively collect `.kt` files from a directory; pass through `.kt` files directly. `.java`
/// inputs are noted as unsupported (krusty has no Java front end yet).
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
    } else if path.ends_with(".kt") {
        out.push(path.to_string());
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
    /// The classpath to drive resolution with: the user's `-cp` entries plus — like kotlinc, unless
    /// `-no-jdk` — the platform JDK's `lib/modules` jimage (the `java.base` bootclasspath). Without it,
    /// kotlin-stdlib symbols whose `@Metadata` references `java/lang/*` (`require`, `String.isNotBlank`,
    /// collection ops, …) fail to resolve — a confusing "unresolved function" whose real cause is a
    /// missing JDK. Resolved from `-jdk-home`, else `$JAVA_HOME`; appended only when the jimage actually
    /// exists, so a misconfigured env never breaks an explicit classpath. Kept out of `parse` so that
    /// stays a pure, env-independent function.
    pub fn effective_classpath(&self) -> Vec<PathBuf> {
        let mut cp = self.classpath.clone();
        if !self.no_jdk {
            if let Some(modules) = platform_jdk_modules(self.jdk_home.as_deref()) {
                cp.push(modules);
            }
        }
        cp
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
  -help                 print this help and exit

Sources may be .kt files or directories (scanned recursively for .kt). Unsupported options are
ignored with a note so existing build invocations keep working.";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Options {
        parse(args.iter().map(|s| s.to_string()))
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
        let o = parse_args(&["-no-jdk", "-jdk-home", "/opt/jdk", "f.kt"]);
        assert_eq!(o.effective_classpath(), o.classpath);
    }

    #[test]
    fn effective_classpath_ignores_a_missing_jdk_home() {
        // A non-existent `-jdk-home` contributes nothing (a bad env must not break an explicit cp).
        let o = parse_args(&["-jdk-home", "/definitely/not/a/jdk", "-cp", "a.jar", "f.kt"]);
        assert_eq!(o.effective_classpath(), vec![PathBuf::from("a.jar")]);
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
