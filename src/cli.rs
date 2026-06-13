//! kotlinc-compatible command-line parsing, so `krusty` can stand in for `kotlinc` in a build:
//! same common flags (`-d`, `-classpath`/`-cp`, `-include-runtime`, `-module-name`, `-jvm-target`,
//! `-version`, `-help`, …), source files **or directories**, `@argfile`s, and graceful handling of
//! options krusty doesn't implement (ignored with a note, rather than treated as source files).

use std::path::PathBuf;

pub struct Options {
    /// Output directory or `.jar` (kotlinc `-d`).
    pub dest: PathBuf,
    /// Classpath entries (dirs/jars).
    pub classpath: Vec<PathBuf>,
    /// `.kt` source files (directories already expanded).
    pub sources: Vec<String>,
    /// Module name → `<module>.kotlin_module` (kotlinc `-module-name`, default `main`).
    pub module_name: String,
    /// Options accepted for compatibility but not acted on (reported once).
    pub ignored: Vec<String>,
    /// `-version` / `-help` requested (handled before compiling).
    pub print_version: bool,
    pub print_help: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            dest: PathBuf::from("krusty-out"),
            classpath: Vec::new(),
            sources: Vec::new(),
            module_name: "main".to_string(),
            ignored: Vec::new(),
            print_version: false,
            print_help: false,
        }
    }
}

/// kotlinc flags that take a following value but which krusty ignores (accept + drop the value).
const IGNORED_WITH_VALUE: &[&str] = &[
    "-jvm-target", "-language-version", "-api-version", "-jdk-home", "-kotlin-home",
    "-jvm-default", "-Xexplicit-api", "-opt-in", "-P", "-script-templates", "-expression", "-e",
];
/// kotlinc valueless flags that krusty ignores (accept + drop).
const IGNORED_FLAGS: &[&str] = &[
    "-include-runtime", "-no-stdlib", "-no-reflect", "-no-jdk", "-nowarn", "-verbose", "-Werror",
    "-progressive", "-script", "-java-parameters", "-Xjvm-default", "-Xuse-ir",
];

/// Split a classpath string on the platform separator (`:` on Unix).
fn split_classpath(v: &str) -> Vec<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    v.split(sep).filter(|s| !s.is_empty()).map(PathBuf::from).collect()
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
            "-version" => opts.print_version = true,
            "-help" | "-h" | "-X" => opts.print_help = true,
            flag if IGNORED_WITH_VALUE.contains(&flag) => {
                let _ = it.next(); // consume + drop the value
                opts.ignored.push(flag.to_string());
            }
            flag if IGNORED_FLAGS.contains(&flag) => opts.ignored.push(flag.to_string()),
            // Unknown option: ignore it (don't mistake it for a source file). kotlinc's `-X...` and
            // `-P...` advanced flags land here.
            flag if flag.starts_with('-') => opts.ignored.push(flag.to_string()),
            // A positional argument: a source file or directory.
            other => collect_sources(other, &mut opts.sources, &mut opts.ignored),
        }
    }
    opts
}

pub const VERSION_LINE: &str = concat!("krusty ", env!("CARGO_PKG_VERSION"), " (kotlinc-compatible Kotlin\u{2192}JVM compiler PoC)");

pub const HELP: &str = "\
usage: krusty [options] <sources>

krusty is a memory-lean Kotlin\u{2192}JVM compiler PoC that aims to be a drop-in for kotlinc on the
supported language subset (kotlinc-equivalent ABI, verified by a differential harness).

Common options (kotlinc-compatible):
  -d <dir|jar>          destination for generated .class files (a directory or a .jar)
  -classpath / -cp <p>  classpath entries (dirs and .jars), ':'-separated
  -module-name <name>   name of the generated <name>.kotlin_module (default: main)
  -include-runtime      accepted (no-op: krusty does not bundle the stdlib)
  -jvm-target <v>        accepted (no-op: krusty currently emits v50 class files)
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
        let o = parse_args(&["-d", "out.jar", "-cp", "a.jar:b/classes", "-module-name", "lib", "x.kt"]);
        assert_eq!(o.dest, PathBuf::from("out.jar"));
        assert_eq!(o.classpath, vec![PathBuf::from("a.jar"), PathBuf::from("b/classes")]);
        assert_eq!(o.module_name, "lib");
        assert_eq!(o.sources, vec!["x.kt".to_string()]);
    }

    #[test]
    fn ignores_unsupported_with_and_without_value() {
        let o = parse_args(&["-include-runtime", "-jvm-target", "1.8", "-Xsomething", "f.kt"]);
        // -jvm-target consumed its value (1.8), not treated as a source.
        assert_eq!(o.sources, vec!["f.kt".to_string()]);
        assert!(o.ignored.contains(&"-include-runtime".to_string()));
        assert!(o.ignored.contains(&"-jvm-target".to_string()));
        assert!(o.ignored.contains(&"-Xsomething".to_string()));
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
