//! The kotlinc-compatible CLI honours compiler-plugin switches instead of dropping them.
//!
//! kotlinc loads the jars named by `-Xplugin=<jar>,<jar>` and configures them with
//! `-P plugin:<id>:<key>=<value>`. krusty cannot execute a JVM FIR/IR plugin, so it resolves each
//! requested jar against its extension registry (`docs/PLUGIN_API.md`): kotlinx.serialization runs as
//! krusty's native pass (with an `info:` line saying so), and every other plugin — all-open, no-arg,
//! Compose — fails the compile. Before, the CLI printed "ignoring unsupported option" and went on to
//! reject all-open's source, emit a no-arg class with no no-arg constructor, and emit an
//! untransformed `@Composable` signature.
//!
//! The reverse holds too: with no plugin requested krusty synthesizes nothing, exactly like kotlinc.
//!
//! A jar is identified the way kotlinc's `ServiceLoader` identifies it — by the registrar class its
//! `META-INF/services` file declares — so the error is proven on a neutral plugin the test builds, and
//! on the real all-open, no-arg and Compose jars wherever the reference distribution ships them.

use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::plugins::registry::PluginDiagnostic;

use super::common;

const SERIALIZABLE_PROBE: &str = "package probe\n\
     import kotlinx.serialization.Serializable\n\
     @Serializable data class P(val x: Int, val tags: List<String> = listOf(\"a\"))\n\
     fun main() = println(P(1))\n";

/// A compiler plugin no registry entry answers to, built by the test: a jar whose service file
/// declares a registrar class, under a name that resembles nothing krusty knows.
fn widget_plugin_jar(dir: &Path) -> PathBuf {
    use std::io::Write;
    let jar = dir.join("widget-compiler-plugin.jar");
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&jar).expect("create the jar"));
    archive
        .start_file(
            "META-INF/services/org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .expect("start the service file");
    archive
        .write_all(b"org.example.widget.WidgetComponentRegistrar\n")
        .expect("write the service file");
    archive.finish().expect("finish the jar");
    jar
}

/// A plugin jar the reference distribution ships, or `None` where this Kotlin version ships none.
fn shipped_plugin_jar(name: &str) -> Option<PathBuf> {
    let jar = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join(name);
    jar.is_file().then_some(jar)
}

fn reference_plugin_jar(name: &str) -> PathBuf {
    let jar = common::kotlinc_lib_dir()
        .expect("the reference kotlinc distribution is provisioned")
        .join(name);
    assert!(
        jar.is_file(),
        "the reference distribution ships {}",
        jar.display()
    );
    jar
}

fn serialization_runtime() -> Vec<PathBuf> {
    vec![
        krusty::toolchain::serialization_core_jar()
            .expect("the kotlinx-serialization-core runtime is provisioned"),
        common::stdlib_jar(),
    ]
}

fn join_classpath(jars: &[PathBuf]) -> String {
    std::env::join_paths(jars)
        .expect("classpath entries contain no separator")
        .to_string_lossy()
        .into_owned()
}

/// The `.class` files under `dir`, as sorted relative paths.
fn class_files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.map(|entry| entry.expect("readable output entry").path()) {
            if entry.is_dir() {
                walk(base, &entry, out);
            } else if entry
                .extension()
                .is_some_and(|extension| extension == "class")
            {
                let relative = entry.strip_prefix(base).expect("entry under the output");
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

struct Compiled {
    code: Option<i32>,
    stderr: String,
    classes: Vec<String>,
}

/// Compile `src` with the krusty binary and with the reference kotlinc, same switches for both.
struct Probe {
    source: PathBuf,
    dir: PathBuf,
}

impl Probe {
    fn new(name: &str, src: &str) -> Probe {
        let dir = common::scratch_dir()
            .expect("a scratch directory")
            .join(format!("cli-plugin-{name}"));
        std::fs::create_dir_all(&dir).expect("create the probe directory");
        let source = dir.join("Probe.kt");
        std::fs::write(&source, src).expect("write the probe source");
        Probe { source, dir }
    }

    fn krusty(&self, classpath: &[PathBuf], switches: &[String]) -> Compiled {
        let out = self.dir.join("krusty");
        let _ = std::fs::remove_dir_all(&out);
        let mut command = Command::new(common::krusty_binary());
        if !classpath.is_empty() {
            command.args(["-cp", &join_classpath(classpath)]);
        }
        let result = command
            .args(switches)
            .arg("-d")
            .arg(&out)
            .arg(&self.source)
            .output()
            .expect("run krusty");
        Compiled {
            code: result.status.code(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
            classes: class_files(&out),
        }
    }

    fn kotlinc_classes(&self, classpath: &[PathBuf], switches: &[String]) -> Vec<String> {
        let out = self.dir.join("kotlinc");
        let _ = std::fs::remove_dir_all(&out);
        let mut arguments = Vec::new();
        if !classpath.is_empty() {
            arguments.extend(["-cp".to_string(), join_classpath(classpath)]);
        }
        arguments.extend(switches.iter().cloned());
        arguments.extend([
            "-d".to_string(),
            out.display().to_string(),
            self.source.display().to_string(),
        ]);
        let (code, diagnostics) =
            common::kotlinc_compile(&arguments).expect("the reference compiler runs");
        assert_eq!(code, 0, "kotlinc rejected the probe: {diagnostics}");
        class_files(&out)
    }
}

fn unsupported(plugin: &str) -> String {
    format!(
        "error: {}\n",
        PluginDiagnostic::Unsupported {
            plugin: plugin.to_string()
        }
        .message()
    )
}

/// A plugin krusty has no implementation of must stop the compile with the registry's error — naming
/// the jar, and the `-P` plugin id when options were passed — rather than compile the module as if
/// the plugin were absent. Proven on a neutral plugin built here, then on all-open, no-arg and Compose.
#[test]
fn a_plugin_krusty_cannot_run_fails_the_compile() {
    let open = "package probe\n\
                annotation class Open\n\
                @Open class Base { fun greet() = \"base\" }\n\
                fun main() = println(Base().greet())\n";
    let widget_dir = Probe::new("widget", open).dir;
    let mut plugins = vec![(
        widget_plugin_jar(&widget_dir),
        Some(("org.example.widget", "annotation=probe.Open")),
    )];
    for (jar_name, options) in [
        (
            "allopen-compiler-plugin.jar",
            Some(("org.jetbrains.kotlin.allopen", "annotation=probe.Open")),
        ),
        (
            "noarg-compiler-plugin.jar",
            Some(("org.jetbrains.kotlin.noarg", "annotation=probe.Open")),
        ),
        ("compose-compiler-plugin.jar", None),
    ] {
        plugins.extend(shipped_plugin_jar(jar_name).map(|jar| (jar, options)));
    }
    for (jar, options) in plugins {
        let label = jar.display().to_string();
        let mut switches = vec![format!("-Xplugin={label}")];
        let mut expected = unsupported(&label);
        if let Some((id, option)) = options {
            switches.extend(["-P".to_string(), format!("plugin:{id}:{option}")]);
            expected.push_str(&unsupported(&format!("plugin id '{id}'")));
        }
        let name = jar
            .file_stem()
            .expect("a jar file name")
            .to_string_lossy()
            .into_owned();
        let probe = Probe::new(&name, open);
        let compiled = probe.krusty(&[], &switches);
        assert_eq!(compiled.stderr, expected, "{label}");
        assert_eq!(compiled.code, Some(1), "{label}: {}", compiled.stderr);
        assert_eq!(compiled.classes, Vec::<String>::new(), "{label}");
    }
}

/// A `-P` option for a plugin id krusty does not know fails the compile even with no jar: the option
/// cannot be honoured, so it is reported, never dropped.
#[test]
fn an_option_for_an_unknown_plugin_id_fails_the_compile() {
    let probe = Probe::new("unknown-id", "fun main() {}\n");
    let compiled = probe.krusty(
        &[],
        &[
            "-P".to_string(),
            "plugin:org.example.widget:level=3".to_string(),
        ],
    );
    assert_eq!(
        compiled.stderr,
        unsupported("plugin id 'org.example.widget'")
    );
    assert_eq!(compiled.code, Some(1));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

/// kotlinc splits `-Xplugin` on commas. A supported and an unsupported plugin in one switch: the
/// serialization jar is substituted (and says so) and the other plugin still fails the compile.
#[test]
fn comma_separated_plugin_jars_are_each_resolved() {
    let serialization = reference_plugin_jar("kotlinx-serialization-compiler-plugin.jar");
    let probe = Probe::new("comma", SERIALIZABLE_PROBE);
    let widget = widget_plugin_jar(&probe.dir);
    let compiled = probe.krusty(
        &serialization_runtime(),
        &[format!(
            "-Xplugin={},{}",
            serialization.display(),
            widget.display()
        )],
    );
    let substituted = PluginDiagnostic::NativeSubstitution {
        plugin_id: krusty::plugins::cli::SERIALIZATION_PLUGIN_ID.to_string(),
        jar: Some(serialization.display().to_string()),
    };
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {}\n{}",
            substituted.message(),
            unsupported(&widget.display().to_string())
        )
    );
    assert_eq!(compiled.code, Some(1));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

/// kotlinc 2.4.20 refuses a plugin jar that does not exist, and so does krusty, in kotlinc's words.
#[test]
fn a_plugin_jar_that_does_not_exist_is_kotlincs_error() {
    let probe = Probe::new("missing", "fun main() {}\n");
    let compiled = probe.krusty(&[], &["-Xplugin=/definitely/not/there.jar".to_string()]);
    assert_eq!(
        compiled.stderr,
        "error: plugin classpath entry points to a non-existent location: \
         /definitely/not/there.jar\n"
    );
    assert_eq!(compiled.code, Some(1));
}

/// The serialization plugin is requested: krusty substitutes its native pass, says so, and emits
/// the same class files kotlinc emits with the real plugin (`P$$serializer`, `P$Companion`).
#[test]
fn the_serialization_plugin_runs_as_krustys_native_pass() {
    let jar = reference_plugin_jar("kotlinx-serialization-compiler-plugin.jar");
    let switches = [format!("-Xplugin={}", jar.display())];
    let classpath = serialization_runtime();
    let probe = Probe::new("serialization", SERIALIZABLE_PROBE);
    let compiled = probe.krusty(&classpath, &switches);
    let substituted = PluginDiagnostic::NativeSubstitution {
        plugin_id: krusty::plugins::cli::SERIALIZATION_PLUGIN_ID.to_string(),
        jar: Some(jar.display().to_string()),
    };
    assert_eq!(
        compiled.stderr,
        format!("info: {}\n", substituted.message())
    );
    assert_eq!(compiled.code, Some(0));
    let reference = probe.kotlinc_classes(&classpath, &switches);
    assert!(
        reference.contains(&"probe/P$$serializer.class".to_string()),
        "{reference:?}"
    );
    assert_eq!(compiled.classes, reference);
}

/// Without the plugin kotlinc synthesizes no serializer for a `@Serializable` class — the
/// annotation is an ordinary runtime annotation — and neither does krusty.
#[test]
fn without_the_serialization_plugin_nothing_is_synthesized() {
    let classpath = serialization_runtime();
    let probe = Probe::new("no-plugin", SERIALIZABLE_PROBE);
    let compiled = probe.krusty(&classpath, &[]);
    assert_eq!(compiled.stderr, "");
    assert_eq!(compiled.code, Some(0));
    let reference = probe.kotlinc_classes(&classpath, &[]);
    assert_eq!(
        reference,
        vec![
            "probe/P.class".to_string(),
            "probe/ProbeKt.class".to_string()
        ]
    );
    assert_eq!(compiled.classes, reference);
}
