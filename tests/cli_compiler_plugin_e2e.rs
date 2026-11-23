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

    /// How a diagnostic names the probe source: relative to the working directory, as kotlinc
    /// renders a file under it.
    fn shown(&self) -> String {
        let working = std::env::current_dir().expect("the working directory");
        self.source
            .strip_prefix(working)
            .unwrap_or(&self.source)
            .display()
            .to_string()
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
    // Compose runs as a partial native pass. A build still tries krusty, which rejects a construct
    // the pass does not cover with its own exit status, so the support check does not refuse it.
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    assert_eq!(
        check(&[format!("-Xplugin={}", compose.display())]),
        (Some(0), String::new(), String::new())
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

/// A build tells a plugin pass's refusal of valid source from a compile error by the exit status
/// alone: 3 for a refusal (kotlinc compiles the module), 1 for an error. `verify-compile` records the
/// refusal as unsupported and the error as a failure.
#[test]
fn a_plugin_refusal_has_its_own_exit_status_and_verify_records_it_as_unsupported() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let switches = vec![format!("-Xplugin={}", compose.display())];
    let verify = |probe: &Probe| {
        let record = probe.dir.join("record.json");
        let reference = probe.dir.join("kotlinc");
        std::fs::create_dir_all(&reference).expect("reference directory");
        let output = Command::new(common::krusty_binary())
            .args(["verify-compile", "--reference"])
            .arg(&reference)
            .arg("--record")
            .arg(&record)
            .args(["--compilation", ":compileKotlin", "--"])
            .args(["-cp", &join_classpath(&classpath)])
            .args(&switches)
            .arg("-d")
            .arg(probe.dir.join("verified"))
            .arg(&probe.source)
            .output()
            .expect("run krusty verify-compile");
        assert_eq!(output.status.code(), Some(0));
        std::fs::read_to_string(record).expect("the record")
    };
    let status = |record: &str| {
        record
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("\"status\":"))
            .expect("the record's status")
            .trim_end_matches(',')
            .to_string()
    };

    let refused = Probe::new(
        "refusal-status",
        "package probe\nimport androidx.compose.runtime.Composable\nimport androidx.compose.runtime.ReusableContent\n@Composable fun Greeting() {\nReusableContent(1) { }\n}\n",
    );
    let compiled = refused.krusty(&classpath, &switches);
    assert_eq!(compiled.code, Some(3), "{}", compiled.stderr);
    assert_eq!(compiled.classes, Vec::<String>::new());
    assert_eq!(status(&verify(&refused)), "\"status\": \"unsupported\"");

    let invalid = Probe::new(
        "error-status",
        "package probe\nimport androidx.compose.runtime.Composable\n@Composable fun Greeting() {}\nval number: Int = \"text\"\n",
    );
    let compiled = invalid.krusty(&classpath, &switches);
    assert_eq!(compiled.code, Some(1), "{}", compiled.stderr);
    assert_eq!(status(&verify(&invalid)), "\"status\": \"failed\"");
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
        42,
    );
    assert_compose_rejects(
        "compose-function-type-with-result",
        "fun keep(content: @Composable () -> Int) {}\n",
        &[],
        "1:1: error: krusty's Compose compiler plugin does not support the composable function \
         type in `keep` yet",
        1,
    );
    assert_compose_rejects(
        "compose-ten-argument-function-type",
        "fun keep(content: @Composable (Int, Int, Int, Int, Int, Int, Int, Int, Int, Int) -> Unit) {}\n",
        &[],
        "1:1: error: krusty's Compose compiler plugin does not support the composable function \
         type in `keep` yet",
        1,
    );
    assert_compose_rejects(
        "compose-ten-input-receiver-function-type",
        "fun keep(content: @Composable Int.(Int, Int, Int, Int, Int, Int, Int, Int, Int) -> Unit) {}\n",
        &[],
        "1:1: error: krusty's Compose compiler plugin does not support the composable function \
         type in `keep` yet",
        1,
    );
    assert_compose_rejects(
        "compose-suspend-function-type",
        "fun keep(content: @Composable suspend () -> Unit) {}\n",
        &[],
        "1:1: error: krusty's Compose compiler plugin does not support the composable function \
         type in `keep` yet",
        1,
    );
    assert_compose_rejects(
        "compose-local-argument",
        "@Composable fun Show(text: String) {}\n@Composable fun Greeting() {\nval text = \"a\"\nShow(text)\n}\n",
        &[],
        "6:1: error: krusty's Compose compiler plugin does not support a composable call argument \
         that is not a constant, a parameter, or computed from a parameter yet",
        10,
    );
    assert_compose_rejects(
        "compose-computed-argument-of-runtime-stability",
        "@Composable fun Show(items: List<Int>) {}\n@Composable fun Greeting(n: Int) {\nShow(listOf(n))\n}\n",
        &[],
        "5:1: error: krusty's Compose compiler plugin does not support a call argument whose type's \
         stability is decided when it runs yet",
        15,
    );
    assert_compose_rejects(
        "compose-lambda-value",
        "@Composable fun Greeting(content: @Composable () -> Unit) {\nval saved = content\n}\n",
        &[],
        "3:13: error: krusty's Compose compiler plugin does not support a composable lambda \
         parameter used other than by calling it or passing it to a composable lambda parameter \
         yet",
        45,
    );
    assert_compose_rejects(
        "compose-plain-lambda-value",
        "fun keep(content: @Composable () -> Unit) {\nval saved = content\n}\n",
        &[],
        "3:1: error: krusty's Compose compiler plugin does not support a composable lambda \
         parameter used other than by calling it or passing it to a composable lambda parameter \
         yet",
        41,
    );
    assert_compose_rejects(
        "compose-plain-lambda-to-plain-parameter",
        "fun keep(content: @Composable () -> Unit) {\nprintln(content)\n}\n",
        &[],
        "3:1: error: krusty's Compose compiler plugin does not support a composable lambda \
         parameter used other than by calling it or passing it to a composable lambda parameter \
         yet",
        41,
    );
    assert_compose_rejects(
        "compose-plain-branch-lambda",
        "fun keep(content: @Composable () -> Unit) {}\nfun f(b: Boolean) {\nif (b) keep {}\n}\n",
        &[],
        "5:13: error: krusty's Compose compiler plugin does not support a composable lambda passed other \
         than by a statement of a function body yet",
        2,
    );
    assert_compose_rejects(
        "compose-plain-expression-body-lambda",
        "fun keep(content: @Composable () -> Unit) {}\nfun f() = keep {}\n",
        &[],
        "4:16: error: krusty's Compose compiler plugin does not support a composable lambda passed other \
         than by a statement of a function body yet",
        2,
    );
    assert_compose_rejects(
        "compose-plain-method-lambda",
        "fun keep(content: @Composable () -> Unit) {}\nclass C {\nfun f() {\nkeep {}\n}\n}\n",
        &[],
        "6:6: error: krusty's Compose compiler plugin does not support a composable lambda passed other \
         than by a statement of a function body yet",
        2,
    );
    assert_compose_rejects(
        "compose-plain-nested-lambda-with-call",
        "@Composable fun Show() {}\nfun keep(content: @Composable () -> Unit) {}\nfun f() {\nrun { keep { Show() } }\n}\n",
        &[],
        "6:12: error: krusty's Compose compiler plugin does not support a composable lambda passed other \
         than by a statement of a function body yet",
        10,
    );
    assert_compose_rejects(
        "compose-capturing-lambda-to-plain-function",
        "@Composable fun Show(text: String) {}\nfun keep(content: @Composable () -> Unit) {}\n\
         @Composable fun Greeting(text: String) {\nkeep { Show(text) }\n}\n",
        &[],
        "6:6: error: krusty's Compose compiler plugin does not support a capturing composable \
         lambda passed from a composable body to a function that is not composable yet",
        14,
    );
    assert_compose_rejects(
        "compose-recursive-lambda",
        "@Composable fun Greeting(content: @Composable () -> Unit) {\nGreeting(content)\n}\n",
        &[],
        "3:13: error: krusty's Compose compiler plugin does not support a recursive composable \
         function whose lambda parameters' applier kotlinc infers yet",
        45,
    );
    let frame = "@Composable fun Frame(content: @Composable () -> Unit) {\ncontent()\n}\n";
    assert_compose_rejects(
        "compose-lambda-passing-captured-local",
        &format!(
            "{frame}@Composable fun Show(text: String) {{}}\n@Composable fun Greeting(text: String) {{\nval shown = text\nFrame {{ Show(shown) }}\n}}\n"
        ),
        &[],
        "9:9: error: krusty's Compose compiler plugin does not support a composable call argument \
         that is not a constant, a parameter, or computed from a parameter yet",
        11,
    );
    assert_compose_rejects(
        "compose-lambda-capturing-lambda-parameter",
        &format!(
            "{frame}@Composable fun Greeting(inner: @Composable () -> Unit) {{\nFrame {{ inner() }}\n}}\n"
        ),
        &[],
        "6:13: error: krusty's Compose compiler plugin does not support a composable lambda that \
         captures a composable lambda parameter yet",
        43,
    );
    assert_compose_rejects(
        "compose-nested-lambda",
        &format!(
            "{frame}@Composable fun Greeting() {{\nFrame {{\nFrame {{ val plain = {{ 1 }} }}\n}}\n}}\n"
        ),
        &[],
        "8:7: error: krusty's Compose compiler plugin does not support a lambda in a composable \
         function yet",
        21,
    );
    assert_compose_rejects(
        "compose-singleton-lambda-remembering-lambda",
        "@Composable fun Show(n: Int) {}\n@Composable fun Each(content: @Composable (Int) -> Unit) {\ncontent(1)\n}\n@Composable fun F() {\nEach { n ->\nEach { _ -> Show(n) }\n}\n}\n",
        &[],
        "8:6: error: krusty's Compose compiler plugin does not support a composable lambda that \
         captures nothing passing a capturing composable lambda yet",
        1,
    );
    assert_compose_rejects(
        "compose-empty-lambda",
        &format!("{frame}@Composable fun Greeting() {{\nFrame {{ }}\n}}\n"),
        &[],
        "7:7: error: krusty's Compose compiler plugin does not support an empty composable lambda \
         yet",
        3,
    );
    assert_compose_rejects(
        "compose-non-skippable",
        "@Composable @androidx.compose.runtime.NonSkippableComposable fun Greeting() {}\n",
        &[],
        "3:62: error: krusty's Compose compiler plugin does not support a \
         `@NonSkippableComposable` function yet",
        14,
    );
    assert_compose_rejects(
        "compose-cross-file-defaults",
        "@Composable fun Greeting() {\nOther()\n}\n",
        &[(
            "Other.kt",
            "package probe\nimport androidx.compose.runtime.Composable\n@Composable fun Other(size: Int = 1) {}\n",
        )],
        "4:1: error: krusty's Compose compiler plugin does not support a call to a composable \
         function of another file with default arguments yet",
        7,
    );
    assert_compose_rejects(
        "compose-library-call-result",
        "@Composable fun Greeting() {\nandroidx.compose.runtime.rememberCompositionContext()\n}\n",
        &[],
        "4:1: error: krusty's Compose compiler plugin does not support a call to a library \
         composable function with a result yet",
        53,
    );
    assert_compose_rejects(
        "compose-library-call-function-parameter",
        "val action: () -> Unit = {}\n@Composable fun Greeting() {\nandroidx.compose.runtime.SideEffect(action)\n}\n",
        &[],
        "5:1: error: krusty's Compose compiler plugin does not support a call to a library \
         composable function with a function-typed parameter that is not a composable function \
         type yet",
        43,
    );
    assert_compose_library_call_rejects(
        "compose-library-call-forwarded-lambda",
        "@Composable fun Frame(content: @Composable () -> Unit) { content() }\n",
        "import lib.Frame\n@Composable fun Wrapper(content: @Composable () -> Unit) {\nFrame(content)\n}\n",
        "5:1: error: krusty's Compose compiler plugin does not support a composable lambda \
         parameter passed to a library composable yet",
        14,
    );
    assert_compose_library_call_rejects(
        "compose-plain-library-forwarded-lambda",
        "fun setContent(content: @Composable () -> Unit) {}\n",
        "import lib.setContent\nfun host(content: @Composable () -> Unit) {\nsetContent(content)\n}\n",
        "5:1: error: krusty's Compose compiler plugin does not support a composable lambda \
         parameter of a function that is not composable passed to a function of another file or \
         a library yet",
        19,
    );
    assert_compose_rejects(
        "compose-generic-value-class-field",
        "@JvmInline value class Id<T>(val raw: T)\nclass User(val id: Id<Int>)\n",
        &[],
        "4:1: error: krusty's Compose compiler plugin does not support a class with a field of a \
         generic value class yet",
        1,
    );
    assert_compose_rejects(
        "compose-reflection-function-field",
        "class Reference(val f: kotlin.reflect.KFunction1<Int, Int>)\n",
        &[],
        "3:1: error: krusty's Compose compiler plugin does not support a class with a field of a \
         numbered reflection function type yet",
        1,
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
        1,
    );
}

/// A compiled composable getter takes the composer and `$changed` after the property's own
/// parameters, as a composable function does. The property still resolves as its declaration
/// states it, whether a class member, a top-level property or an extension, and the pass hands a
/// module that reads one to kotlinc, since it does not pass a getter those parameters yet.
#[test]
fn a_read_of_a_composable_property_resolves_and_the_compose_pass_refuses_it() {
    assert_compose_rejects(
        "compose-member-property-read",
        "val LocalName = androidx.compose.runtime.compositionLocalOf { \"a\" }\n\
         @Composable fun Greeting(): String = LocalName.current\n",
        &[],
        "4:38: error: krusty's Compose compiler plugin does not support a read of a composable \
         property yet",
        17,
    );
    assert_compose_rejects(
        "compose-top-level-property-read",
        "import androidx.compose.runtime.currentCompositeKeyHash\n\
         @Composable fun Key(): Int = currentCompositeKeyHash\n",
        &[],
        "4:30: error: krusty's Compose compiler plugin does not support a read of a composable \
         property yet",
        23,
    );
    assert_compose_rejects(
        "compose-extension-property-read",
        "import androidx.compose.foundation.layout.WindowInsets\n\
         import androidx.compose.foundation.layout.safeDrawing\n\
         @Composable fun Insets(): WindowInsets = WindowInsets.safeDrawing\n",
        &[],
        "5:42: error: krusty's Compose compiler plugin does not support a read of a composable \
         property yet",
        24,
    );
}

/// A library composable's default argument is realized by its transformed entry, which only the
/// Compose plugin's calls pass the mask to: a module compiled without the plugin that omits one
/// gets the frontend's error for a default nothing can realize.
#[test]
fn a_library_composable_default_is_unavailable_without_the_compose_plugin() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let library = Probe::new(
        "compose-default-without-plugin-lib",
        "package lib\nimport androidx.compose.runtime.Composable\n\
         @Composable fun Sized(size: Int = 1) {}\n",
    );
    let built = library.krusty(&classpath, &[format!("-Xplugin={}", compose.display())]);
    assert_eq!(built.code, Some(0), "{}", built.stderr);
    classpath.push(library.dir.join("krusty"));
    let probe = Probe::new(
        "compose-default-without-plugin",
        "package probe\nimport lib.Sized\nfun plain() { Sized() }\n",
    );
    let compiled = probe.krusty(&classpath, &[]);
    assert_eq!(
        compiled.stderr,
        format!(
            "{}:3:15: error: default-argument realization is unavailable for 'Sized'.\n\
             fun plain() {{ Sized() }}\n\
             {}{}\n\
             krusty: 1 error(s)\n",
            probe.shown(),
            " ".repeat(14),
            "^".repeat(5)
        )
    );
    assert_eq!(compiled.code, Some(1));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

/// A call to a composable function of another file compiles through krusty's partial pass: the
/// class set is the one kotlinc emits for the same two files.
#[test]
fn the_compose_pass_compiles_a_call_to_a_composable_function_of_another_file() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let probe = Probe::new(
        "compose-cross-file",
        "package probe\nimport androidx.compose.runtime.Composable\n\
         @Composable fun Greeting() {\nOther()\n}\n",
    );
    let other = probe.dir.join("Other.kt");
    std::fs::write(
        &other,
        "package probe\nimport androidx.compose.runtime.Composable\n@Composable fun Other() {}\n",
    )
    .expect("write the other source");
    let switches = vec![
        format!("-Xplugin={}", compose.display()),
        other.display().to_string(),
    ];
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    }
    .message();
    assert_eq!(compiled.stderr, format!("info: {partial}\n"));
    assert_eq!(compiled.code, Some(0));
    let reference = probe.kotlinc_classes(&classpath, &switches);
    assert_eq!(compiled.classes, reference);
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

/// Compiled directly, an atomicfu module compiles through krusty's partial pass, which rejects each
/// atomic it does not lower rather than leave the library's wrapper in its place.
#[test]
fn the_atomicfu_pass_rejects_what_it_does_not_cover() {
    assert_atomicfu_rejects(
        "atomicfu-boolean",
        "class Flag { private val set = atomic(false) }\n",
        "3:22: error: krusty's atomicfu compiler plugin does not support an AtomicBoolean yet",
        23,
    );
    assert_atomicfu_rejects(
        "atomicfu-top-level",
        "private val top = atomic(0)\nfun next() = top.incrementAndGet()\n",
        "3:9: error: krusty's atomicfu compiler plugin does not support a top-level atomic \
         property yet",
        19,
    );
    assert_atomicfu_rejects(
        "atomicfu-update",
        "class Counter {\nprivate val hits = atomic(0)\nfun next() = hits.updateAndGet { it + 1 }\n}\n",
        "5:14: error: krusty's atomicfu compiler plugin does not support the atomic operation \
         `updateAndGet` yet",
        28,
    );
    assert_atomicfu_rejects(
        "atomicfu-value",
        "class Counter {\nprivate val hits = atomic(0)\nfun wrapper(): Any = hits\n}\n",
        "5:22: error: krusty's atomicfu compiler plugin does not support an atomic property used \
         as a value yet",
        4,
    );
    assert_atomicfu_rejects(
        "atomicfu-var",
        "class Counter {\nprivate var hits = atomic(0)\nfun reset() { hits = atomic(1) }\n}\n",
        "4:9: error: krusty's atomicfu compiler plugin does not support a mutable atomic property \
         yet",
        20,
    );
    assert_atomicfu_rejects(
        "atomicfu-inner",
        "class Counter {\nprivate val hits = atomic(0)\ninner class Probe { fun next() = hits.incrementAndGet() }\n}\n",
        "5:34: error: krusty's atomicfu compiler plugin does not support an atomic operation \
         outside a member function of the atomic's class yet",
        22,
    );
}

/// The line an error names, and a run of `width` carets under it, as kotlinc's renderer prints a
/// diagnostic's source. `error` starts with the `line:column` the diagnostic is reported at.
fn refusal_excerpt(source: &str, error: &str, width: usize) -> String {
    let mut position = error.split(':');
    let line: usize = position.next().unwrap().parse().expect("the error's line");
    let column: usize = position
        .next()
        .unwrap()
        .parse()
        .expect("the error's column");
    format!(
        "{}\n{}{}",
        source
            .lines()
            .nth(line - 1)
            .expect("the error's line exists"),
        " ".repeat(column - 1),
        "^".repeat(width)
    )
}

fn assert_atomicfu_rejects(name: &str, declarations: &str, error: &str, width: usize) {
    let plugin = krusty::toolchain::atomicfu_compiler_plugin_jar()
        .expect("the atomicfu compiler plugin is provisioned");
    let source = format!("package probe\nimport kotlinx.atomicfu.*\n{declarations}");
    let probe = Probe::new(name, &source);
    let classpath = vec![
        krusty::toolchain::atomicfu_runtime_jar().expect("the atomicfu runtime is provisioned"),
        common::stdlib_jar(),
    ];
    let compiled = probe.krusty(&classpath, &[format!("-Xplugin={}", plugin.display())]);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::ATOMICFU_PLUGIN_ID.to_string(),
    }
    .message();
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {partial}\n{}:{error}; compile this module with kotlinc\n{}\nkrusty: 1 error(s)\n",
            probe.shown(),
            refusal_excerpt(&source, error, width)
        )
    );
    assert_eq!(compiled.code, Some(3));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

/// Compile `declarations` (after a package line and the `Composable` import) and the `others`
/// files with the Compose plugin, and assert the one error `Probe.kt:<error>` krusty reports.
/// Like [`assert_compose_rejects`], with a call into `library`, a module krusty compiles first
/// with the Compose plugin in package `lib`.
fn assert_compose_library_call_rejects(
    name: &str,
    library: &str,
    declarations: &str,
    error: &str,
    width: usize,
) {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let switches = vec![format!("-Xplugin={}", compose.display())];
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let library = Probe::new(
        &format!("{name}-lib"),
        &format!("package lib\nimport androidx.compose.runtime.Composable\n{library}"),
    );
    let built = library.krusty(&classpath, &switches);
    assert_eq!(built.code, Some(0), "{}", built.stderr);
    classpath.push(library.dir.join("krusty"));
    let source =
        format!("package probe\nimport androidx.compose.runtime.Composable\n{declarations}");
    let probe = Probe::new(name, &source);
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    }
    .message();
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {partial}\n{}:{error}; compile this module with kotlinc\n{}\nkrusty: 1 error(s)\n",
            probe.shown(),
            refusal_excerpt(&source, error, width)
        )
    );
    assert_eq!(compiled.code, Some(3));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

/// A library composable that takes a `@Composable () -> Unit` is compiled with the composer and
/// `$changed` after a `Function2` value, so the declaration's source parameters still align with
/// its method: the call resolves, and the pass alone declines the lambda it passes.
#[test]
fn a_library_composable_lambda_parameter_aligns_with_its_method() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let source = "package probe\nimport androidx.compose.runtime.Composable\n\
                  import androidx.compose.runtime.ReusableContent\n\
                  @Composable fun Greeting() {\nReusableContent(1) { }\n}\n";
    let probe = Probe::new("compose-library-lambda-parameter", source);
    let switches = vec![format!("-Xplugin={}", compose.display())];
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    }
    .message();
    let file = probe.shown();
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {partial}\n\
             {file}:4:13: error: krusty's Compose compiler plugin does not support a lambda in a \
             composable function yet; compile this module with kotlinc\n\
             @Composable fun Greeting() {{\n\
             {}{}\n\
             {file}:5:1: error: krusty's Compose compiler plugin does not support a composable call \
             outside the statements of a composable body yet; compile this module with kotlinc\n\
             ReusableContent(1) {{ }}\n\
             {}\n\
             krusty: 2 error(s)\n",
            " ".repeat(12),
            "^".repeat(14),
            "^".repeat(22)
        )
    );
    assert_eq!(compiled.code, Some(3));
}

/// A composable lambda parameter passed on to a library composable needs the applier scheme of the
/// library's parameter, which the pass does not infer: the call is declined, whole, against a library
/// the reference compiler and its plugin compiled.
#[test]
fn forwarding_a_composable_lambda_parameter_to_a_producer_library_is_declined() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let switches = vec![format!("-Xplugin={}", compose.display())];
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let producer = Probe::new(
        "compose-forwarding-lib",
        "package lib\nimport androidx.compose.runtime.Composable\n\
         @Composable fun Frame(content: @Composable () -> Unit) { content() }\n",
    );
    producer.kotlinc_classes(&classpath, &switches);
    classpath.push(producer.dir.join("kotlinc"));
    let probe = Probe::new(
        "compose-forwarding",
        "package probe\nimport androidx.compose.runtime.Composable\nimport lib.Frame\n\
         @Composable fun Wrapper(content: @Composable () -> Unit) {\nFrame(content)\n}\n",
    );
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    }
    .message();
    assert_eq!(
        compiled.stderr,
        format!(
            "info: {partial}\n{}:5:1: error: krusty's Compose compiler plugin does not support a \
             composable lambda parameter passed to a library composable yet; compile this module \
             with kotlinc\nFrame(content)\n{}\nkrusty: 1 error(s)\n",
            probe.shown(),
            "^".repeat(14)
        )
    );
    assert_eq!(compiled.code, Some(3));
}

/// The widths around the Compose compiler's changed-word boundaries (nine and ten parameters, and
/// the last width with a `Function` class of its own) of a `@Composable` function type, taken from
/// a library the reference compiler and its plugin compiled.
const COMPOSABLE_WIDTHS: [usize; 6] = [0, 9, 10, 11, 19, 20];

fn composable_function_type(parameters: usize) -> String {
    format!("({})", vec!["Int"; parameters].join(", "))
}

/// Every width aligns with the descriptor the producer emitted: metadata keeps the one source
/// parameter, and no neighbouring width of the same declaration does.
#[test]
fn a_library_composable_function_parameter_aligns_at_every_changed_word_boundary() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let switches = vec![format!("-Xplugin={}", compose.display())];
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let mut library = String::from("package lib\nimport androidx.compose.runtime.Composable\n");
    for width in COMPOSABLE_WIDTHS {
        let ty = composable_function_type(width);
        library.push_str(&format!(
            "@Composable fun Wide{width}(content: @Composable {ty} -> Unit) {{}}\n\
             fun Plain{width}(content: {ty} -> Unit) {{}}\n"
        ));
    }
    let producer = Probe::new("compose-widths-lib", &library);
    producer.kotlinc_classes(&classpath, &switches);
    classpath.push(producer.dir.join("kotlinc"));
    let classpath = krusty::jvm::classpath::Classpath::new(classpath);
    let facade = krusty::types::type_name("lib/ProbeKt");
    let class = classpath
        .find_name(facade)
        .expect("the producer's facade class");
    // The source parameters metadata keeps for `declaration` aligned with the descriptor the
    // producer emitted for `method`.
    let kept = |declaration: &str, method: &str| {
        let method = class
            .methods
            .iter()
            .find(|candidate| candidate.name == method)
            .unwrap_or_else(|| panic!("the producer emits {method}"));
        let (parameters, ret) =
            krusty::jvm::jvm_libraries::parse_method_desc(&method.descriptor).expect("descriptor");
        classpath
            .metadata_call_facts_name(facade, declaration, &parameters, &ret, false, &|_| None)
            .kept_params
    };
    for width in COMPOSABLE_WIDTHS {
        let (composable, plain) = (format!("Wide{width}"), format!("Plain{width}"));
        assert_eq!(kept(&composable, &composable), Some(1), "{composable}");
        assert_eq!(kept(&plain, &plain), Some(1), "{plain}");
        // Each declaration refuses the other's erasure: the composable one has the composer and
        // changed words, the plain one has none.
        assert_eq!(kept(&composable, &plain), None, "{composable} as {plain}");
        assert_eq!(kept(&plain, &composable), None, "{plain} as {composable}");
    }
}

/// A function type without the annotation keeps the source arity at the width where the
/// composable erasure gains a second changed word, so its call resolves and compiles.
#[test]
fn a_function_parameter_aligns_by_the_erasure_its_producer_emitted() {
    let compose = reference_plugin_jar("compose-compiler-plugin.jar");
    let mut classpath =
        krusty::toolchain::compose_runtime_jars().expect("the Compose runtime is provisioned");
    classpath.push(common::stdlib_jar());
    let ty = composable_function_type(10);
    let plain = Probe::new(
        "compose-widths-plain-lib",
        &format!("package lib\nfun Plain(content: {ty} -> Unit) {{}}\n"),
    );
    plain.kotlinc_classes(&classpath, &[]);
    classpath.push(plain.dir.join("kotlinc"));
    let probe = Probe::new(
        "compose-widths-plain",
        &format!("package probe\nfun use(content: {ty} -> Unit) {{\nlib.Plain(content)\n}}\n"),
    );
    let switches = vec![format!("-Xplugin={}", compose.display())];
    let compiled = probe.krusty(&classpath, &switches);
    let partial = PluginDiagnostic::PartialSubstitution {
        plugin_id: krusty::plugins::cli::COMPOSE_PLUGIN_ID.to_string(),
    }
    .message();
    assert_eq!(compiled.stderr, format!("info: {partial}\n"));
    assert_eq!(compiled.code, Some(0));
}

fn assert_compose_rejects(
    name: &str,
    declarations: &str,
    others: &[(&str, &str)],
    error: &str,
    width: usize,
) {
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
            "info: {partial}\n{}:{error}; compile this module with kotlinc\n{}\nkrusty: 1 error(s)\n",
            probe.shown(),
            refusal_excerpt(&source, error, width)
        )
    );
    assert_eq!(compiled.code, Some(3));
    assert_eq!(compiled.classes, Vec::<String>::new());
}

#[path = "cli_compiler_plugin_e2e/atomic_visibility.rs"]
mod atomic_visibility;
