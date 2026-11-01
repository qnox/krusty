//! The kotlinc-compatible CLI honours compiler-plugin switches instead of dropping them.
//!
//! kotlinc loads the jars named by `-Xplugin=<jar>,<jar>` and configures them with
//! `-P plugin:<id>:<key>=<value>`. krusty cannot execute a JVM FIR/IR plugin, so it resolves each
//! requested jar against its extension registry (`docs/PLUGIN_API.md`): kotlinx.serialization runs as
//! krusty's native pass (with an `info:` line saying so), and so do all-open
//! (`allopen_plugin_e2e`), no-arg (`noarg_plugin_e2e`) and sam-with-receiver
//! (`sam_with_receiver_plugin_e2e`). Compose runs as a partial native pass that a build is still
//! refused (`compose_plugin_e2e`); every other plugin fails the compile. Before, the CLI printed
//! "ignoring unsupported option" and went on to emit a no-arg class with no no-arg constructor.
//!
//! The reverse holds too: with no plugin requested krusty synthesizes nothing, exactly like kotlinc.
//!
//! A jar is identified the way kotlinc's `ServiceLoader` identifies it — by the registrar class its
//! `META-INF/services` file declares — so the error is proven on a neutral plugin the test builds.

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
/// the plugin were absent. Proven on a neutral plugin built here.
#[test]
fn a_plugin_krusty_cannot_run_fails_the_compile() {
    let open = "package probe\n\
                annotation class Open\n\
                @Open class Base { fun greet() = \"base\" }\n\
                fun main() = println(Base().greet())\n";
    let widget_dir = Probe::new("widget", open).dir;
    let plugins = [(
        widget_plugin_jar(&widget_dir),
        Some(("org.example.widget", "annotation=probe.Open")),
    )];
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

/// The Kotlin Gradle plugin hands kotlinc its default scripting plugin on every JVM compile, next to
/// the serialization plugin when that is applied, and passes the plugin together with the scripting
/// libraries it loads: kotlinc's own compiler classpath does not carry them. The scripting plugin
/// acts on script sources only, so krusty accepts it silently and emits the class set kotlinc emits
/// for the same command line.
#[test]
fn kotlincs_default_scripting_plugin_is_accepted_beside_serialization() {
    let serialization = reference_plugin_jar("kotlinx-serialization-compiler-plugin.jar");
    let plugins = std::iter::once(serialization.clone())
        .chain(
            [
                "kotlin-scripting-compiler.jar",
                "kotlin-scripting-compiler-impl.jar",
                "kotlin-scripting-common.jar",
                "kotlin-scripting-jvm.jar",
            ]
            .map(reference_plugin_jar),
        )
        .map(|jar| jar.display().to_string())
        .collect::<Vec<_>>();
    let switches = [format!("-Xplugin={}", plugins.join(","))];
    let classpath = serialization_runtime();
    let probe = Probe::new("scripting", SERIALIZABLE_PROBE);
    let compiled = probe.krusty(&classpath, &switches);
    let substituted = PluginDiagnostic::NativeSubstitution {
        plugin_id: krusty::plugins::cli::SERIALIZATION_PLUGIN_ID.to_string(),
        jar: Some(serialization.display().to_string()),
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

/// `krusty refusals <arguments>` answers, before any source is read, whether krusty compiles the
/// request itself: a build tool hands a refused request to kotlinc. Each refusal is the error a
/// compile with the same switches reports, one per line on stdout, and the exit code is 1; a request
/// krusty implements prints nothing and exits 0.
#[test]
fn refusals_lists_the_arguments_and_plugins_krusty_refuses() {
    let probe = Probe::new("plugin-check", "fun main() {}\n");
    let widget = widget_plugin_jar(&probe.dir);
    let serialization = reference_plugin_jar("kotlinx-serialization-compiler-plugin.jar");
    let scripting = reference_plugin_jar("kotlin-scripting-compiler.jar");
    let check = |switches: &[String]| {
        let output = Command::new(common::krusty_binary())
            .arg("refusals")
            .args(switches)
            .output()
            .expect("run krusty refusals");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let refused = |plugin: &str| {
        format!(
            "{}\n",
            PluginDiagnostic::Unsupported {
                plugin: plugin.to_string()
            }
            .message()
        )
    };

    assert_eq!(
        check(&[format!(
            "-Xplugin={},{}",
            serialization.display(),
            scripting.display()
        )]),
        (Some(0), String::new(), String::new())
    );
    assert_eq!(check(&[]), (Some(0), String::new(), String::new()));
    assert_eq!(
        check(&[
            format!("-Xplugin={},{}", serialization.display(), widget.display()),
            "-P".to_string(),
            "plugin:org.example.widget:annotation=probe.Open".to_string(),
        ]),
        (
            Some(1),
            format!(
                "{}{}",
                refused(&widget.display().to_string()),
                refused("plugin id 'org.example.widget'")
            ),
            String::new()
        )
    );
    assert_eq!(
        check(&[
            "-Xvalidate-bytecode".to_string(),
            format!("-Xplugin={}", widget.display()),
        ]),
        (
            Some(1),
            format!(
                "krusty does not implement the kotlinc argument '-Xvalidate-bytecode'\n{}",
                refused(&widget.display().to_string())
            ),
            String::new()
        )
    );
    // Compose runs as a partial native pass, which a build is refused until it is complete.
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    assert_eq!(
        check(&[format!("-Xplugin={}", compose.display())]),
        (
            Some(1),
            format!(
                "{}\n",
                PluginDiagnostic::PartialSubstitution {
                    plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string()
                }
                .message()
            ),
            String::new()
        )
    );
    // So is a Kotlin release krusty does not support: kotlinc of that release compiles it.
    assert_eq!(
        check(&["-Xkotlin-reference-version=2.3.0".to_string()]),
        (
            Some(1),
            format!(
                "-Xkotlin-reference-version=2.3.0 names no supported Kotlin release (supported: {})\n",
                krusty::kotlin_version::KotlinVersion::supported()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            String::new()
        )
    );
    // An output shape krusty does not emit is refused like an argument it does not implement.
    assert_eq!(
        check(&["-Xassertions=jvm".to_string()]),
        (
            Some(1),
            "-Xassertions=jvm selects an output shape krusty does not emit\n".to_string(),
            String::new()
        )
    );
    // A value kotlinc itself rejects is not a refusal: the check fails with the errors the
    // compilation reports, in its order, so neither compiler runs.
    assert_eq!(
        check(&[
            "-Xexplicit-api=sideways".to_string(),
            "-Xvalidate-bytecode".to_string(),
        ]),
        (
            Some(2),
            String::new(),
            "krusty: error: unknown value for parameter -Xexplicit-api: 'sideways'. Value should be one \
             of {disable, strict, warning}\n\
             krusty: error: krusty does not implement the kotlinc argument '-Xvalidate-bytecode'\n"
                .to_string()
        )
    );
    for argument in ["-Xlambdas", "-Xsam-conversions"] {
        assert_eq!(
            check(&[format!("{argument}=nonesuch")]),
            (
                Some(2),
                String::new(),
                format!(
                    "krusty: error: {argument}=nonesuch selects an output shape krusty does not emit\n"
                )
            ),
            "{argument}"
        );
    }
    // A configuration error fails the check with the report the compilation fails with.
    assert_eq!(
        check(&[
            "-Xfragments=main".to_string(),
            "-Xfragment-sources=broken".to_string(),
        ]),
        (
            Some(2),
            String::new(),
            "error: incorrect syntax for -Xfragment-sources argument. `<module name>:<source file>` \
             expected but got `broken`\n"
                .to_string()
        )
    );
    assert_eq!(
        check(&["-XXLanguage:-TrailingCommas".to_string()]),
        (
            Some(2),
            String::new(),
            "warning: ATTENTION!\n\
             This build uses unsafe internal compiler arguments:\n\
             \n\
             -XXLanguage:-TrailingCommas\n\
             \n\
             This mode is not recommended for production use,\n\
             as no stability/compatibility guarantees are given on\n\
             compiler or generated code. Use it at your own risk!\n\
             \n\
             error: the following features cannot be disabled manually, because the version they \
             first appeared in is no longer supported:\n\
             TrailingCommas\n"
                .to_string()
        )
    );
}

/// Compiled directly, a Compose module compiles through krusty's partial pass, which rejects each
/// composable construct it does not cover rather than emit it untransformed.
#[test]
fn the_compose_pass_rejects_what_it_does_not_cover() {
    assert_compose_rejects(
        "compose-default-argument",
        "@Composable fun Greeting(name: String = \"a\".repeat(2)) {}\n",
        &[],
        "3:13: error: krusty's Compose compiler plugin does not support a composable function \
         with a default argument that is not a constant yet",
    );
    assert_compose_rejects(
        "compose-inferred-stability",
        "@Composable fun Greeting(names: List<String>) {}\n",
        &[],
        "3:13: error: krusty's Compose compiler plugin does not support a composable function \
         parameter whose stability kotlinc infers yet",
    );
    assert_compose_rejects(
        "compose-local-argument",
        "@Composable fun Show(text: String) {}\n@Composable fun Greeting() {\nval text = \"a\"\nShow(text)\n}\n",
        &[],
        "6:1: error: krusty's Compose compiler plugin does not support a composable call argument \
         that is not a constant, a parameter, or computed from a parameter yet",
    );
    assert_compose_rejects(
        "compose-lambda-value",
        "@Composable fun Greeting(content: @Composable () -> Unit) {\nval saved = content\n}\n",
        &[],
        "3:13: error: krusty's Compose compiler plugin does not support a composable lambda \
         parameter used other than by calling it or passing it to a composable call yet",
    );
    assert_compose_rejects(
        "compose-recursive-lambda",
        "@Composable fun Greeting(content: @Composable () -> Unit) {\nGreeting(content)\n}\n",
        &[],
        "3:13: error: krusty's Compose compiler plugin does not support a recursive composable \
         function whose lambda parameters' applier kotlinc infers yet",
    );
    let frame = "@Composable fun Frame(content: @Composable () -> Unit) {\ncontent()\n}\n";
    assert_compose_rejects(
        "compose-lambda-capturing-local",
        &format!(
            "{frame}@Composable fun Greeting(text: String) {{\nval shown = text\nFrame {{ println(shown) }}\n}}\n"
        ),
        &[],
        "6:13: error: krusty's Compose compiler plugin does not support a composable lambda that \
         captures a local value yet",
    );
    assert_compose_rejects(
        "compose-lambda-capturing-lambda-parameter",
        &format!(
            "{frame}@Composable fun Greeting(inner: @Composable () -> Unit) {{\nFrame {{ inner() }}\n}}\n"
        ),
        &[],
        "6:13: error: krusty's Compose compiler plugin does not support a composable lambda that \
         captures a composable lambda parameter yet",
    );
    assert_compose_rejects(
        "compose-nested-lambda",
        &format!(
            "{frame}@Composable fun Greeting() {{\nFrame {{\nFrame {{ Frame {{ }} }}\n}}\n}}\n"
        ),
        &[],
        "6:13: error: krusty's Compose compiler plugin does not support a lambda in a composable \
         lambda yet",
    );
    assert_compose_rejects(
        "compose-empty-lambda",
        &format!("{frame}@Composable fun Greeting() {{\nFrame {{ }}\n}}\n"),
        &[],
        "7:7: error: krusty's Compose compiler plugin does not support an empty composable lambda \
         yet",
    );
    assert_compose_rejects(
        "compose-non-skippable",
        "@Composable @androidx.compose.runtime.NonSkippableComposable fun Greeting() {}\n",
        &[],
        "3:62: error: krusty's Compose compiler plugin does not support a \
         `@NonSkippableComposable` function yet",
    );
    assert_compose_rejects(
        "compose-cross-file",
        "@Composable fun Greeting() {\nOther()\n}\n",
        &[(
            "Other.kt",
            "package probe\nimport androidx.compose.runtime.Composable\n@Composable fun Other() {}\n",
        )],
        "4:1: error: krusty's Compose compiler plugin does not support a call to a composable \
         function from another file yet",
    );
    assert_compose_rejects(
        "compose-generic-class-field",
        "class Box<T>(val value: T)\nclass User(val box: Box<Int>)\n",
        &[],
        "4:1: error: krusty's Compose compiler plugin does not support a class whose stability \
         depends on a generic class yet",
    );
    assert_compose_rejects(
        "compose-generic-value-class-field",
        "@JvmInline value class Id<T>(val raw: T)\nclass User(val id: Id<Int>)\n",
        &[],
        "4:1: error: krusty's Compose compiler plugin does not support a class with a field of a \
         generic value class yet",
    );
    assert_compose_rejects(
        "compose-reflection-function-field",
        "class Reference(val f: kotlin.reflect.KFunction1<Int, Int>)\n",
        &[],
        "3:1: error: krusty's Compose compiler plugin does not support a class with a field of a \
         numbered reflection function type yet",
    );
    assert_compose_rejects(
        "compose-library-generic-class-field",
        "class Holder(val value: androidx.compose.runtime.ProvidedValue<Int>)\n",
        &[],
        "3:1: error: krusty's Compose compiler plugin does not support a class whose stability \
         depends on a generic class of another file or a Compose library yet",
    );
    assert_compose_rejects(
        "compose-other-file-inner-class-field",
        "class Card(val inner: Outer.Inner)\n",
        &[(
            "Outer.kt",
            "package probe\nclass Outer { inner class Inner(val x: Int) }\n",
        )],
        "3:1: error: krusty's Compose compiler plugin does not support a class whose stability \
         depends on another file's class without `$stable` yet",
    );
}

/// Compose's metrics and reports name files krusty does not write yet: a nonempty destination fails
/// the compile, naming each option, and nothing is emitted. An empty one requests no report.
#[test]
fn compose_report_destinations_fail_the_compile() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let probe = Probe::new(
        "compose-reports",
        "package probe\nimport androidx.compose.runtime.Composable\n@Composable fun Greeting() {}\n",
    );
    let option = |key: &str, value: &str| krusty::plugins::cli::PluginOption {
        id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
        key: key.to_string(),
        value: value.to_string(),
    };
    let mut switches = vec![format!("-Xplugin={}", compose.display())];
    for (key, value) in [
        ("metricsDestination", "build/metrics"),
        ("reportsDestination", "build/reports"),
        ("reportsDestination", ""),
    ] {
        let option = option(key, value);
        switches.extend([
            "-P".to_string(),
            format!("plugin:{}:{}={}", option.id, option.key, option.value),
        ]);
    }
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    };
    let unimplemented = |key: &str, value: &str| {
        PluginDiagnostic::UnimplementedOption {
            option: option(key, value),
        }
        .message()
    };
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {}\nerror: {}\nerror: {}\n",
            partial.message(),
            unimplemented("metricsDestination", "build/metrics"),
            unimplemented("reportsDestination", "build/reports"),
        )
    );
    assert_eq!(compiled.code, Some(1));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

/// Compile `declarations` (after a package line and the `Composable` import) and the `others`
/// files with the Compose plugin, and assert the one error `Probe.kt:<error>` krusty reports.
fn assert_compose_rejects(name: &str, declarations: &str, others: &[(&str, &str)], error: &str) {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let source =
        format!("package probe\nimport androidx.compose.runtime.Composable\n{declarations}");
    let probe = Probe::new(name, &source);
    let mut switches = vec![format!("-Xplugin={}", compose.display())];
    for (file, text) in others {
        let path = probe.dir.join(file);
        std::fs::write(&path, text).expect("write the other source");
        switches.push(path.display().to_string());
    }
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    }
    .message();
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {partial}\n{}:{error}; compile this module with kotlinc\nkrusty: 1 error(s)\n",
            probe.source.display()
        )
    );
    assert_eq!(compiled.code, Some(1));
    assert_eq!(compiled.classes, Vec::<String>::new());
}
