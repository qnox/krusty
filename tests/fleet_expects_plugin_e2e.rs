//! JetBrains Fleet's `expects` compiler plugin, as krusty's native pass.
//!
//! Fleet links a function whose whole body is `fleet.util.multiplatform.linkToActual()` to the
//! `@fleet.util.multiplatform.Actual` function of the same package named like it plus `Jvm`. The
//! plugin is not part of the Kotlin distribution, so the test builds it from its sources
//! (`tests/fixtures/fleet_expects/plugin`, copied from intellij-community's
//! `fleet/compiler-plugins/expects`) with the reference kotlinc against `kotlin-compiler.jar`, and
//! the runtime declaring `linkToActual` and `@Actual` from `fleet/util/multiplatform`. kotlinc runs
//! the real plugin; krusty recognizes it by its registrar and substitutes its native pass. Every
//! class must be identical byte for byte, and the plugin's errors identical in position, message
//! and order.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use super::common;
use super::compiler_plugin_fixture::{assert_same_classes_and_box, PluginFixture};

const PLUGIN_SOURCES: [(&str, &str); 2] = [
    (
        "ExpectsPluginIrGenerationExtension.kt",
        include_str!("fixtures/fleet_expects/plugin/ExpectsPluginIrGenerationExtension.kt"),
    ),
    (
        "ExpectsPluginRegistrar.kt",
        include_str!("fixtures/fleet_expects/plugin/ExpectsPluginRegistrar.kt"),
    ),
];

const PLUGIN_SERVICE: &str = include_str!(
    "fixtures/fleet_expects/plugin/META-INF/services/org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar"
);

const RUNTIME_SOURCE: &str = include_str!("fixtures/fleet_expects/runtime/ExpectInMultiplatform.kt");

/// The plugin and runtime class directories, built once per test process.
struct FleetBuild {
    plugin: PathBuf,
    runtime: PathBuf,
}

fn compile_with_kotlinc(arguments: Vec<String>, what: &str) {
    let (code, diagnostics) =
        common::kotlinc_server::kotlinc_compile_unrecorded(&arguments).expect("the reference compiler runs");
    assert_eq!(code, 0, "kotlinc rejected the {what}: {diagnostics}");
}

fn write_sources(dir: &Path, sources: &[(&str, &str)]) -> Vec<String> {
    std::fs::create_dir_all(dir).expect("create a source directory");
    sources
        .iter()
        .map(|(name, text)| {
            let path = dir.join(name);
            std::fs::write(&path, text).expect("write a source");
            path.display().to_string()
        })
        .collect()
}

fn fleet_build() -> &'static FleetBuild {
    static BUILD: OnceLock<FleetBuild> = OnceLock::new();
    BUILD.get_or_init(|| {
        let dir = common::scratch_dir()
            .expect("a scratch directory")
            .join("fleet-expects-plugin");
        let _ = std::fs::remove_dir_all(&dir);
        let compiler =
            common::kotlin_compiler_jar().expect("the reference kotlinc distribution is provisioned");
        let plugin = dir.join("plugin");
        let mut arguments = vec![
            "-opt-in=org.jetbrains.kotlin.config.MessageCollectorAccess".to_string(),
            "-cp".to_string(),
            compiler.display().to_string(),
            "-d".to_string(),
            plugin.display().to_string(),
        ];
        arguments.extend(write_sources(&dir.join("plugin-src"), &PLUGIN_SOURCES));
        compile_with_kotlinc(arguments, "expects plugin");
        let services = plugin.join("META-INF/services");
        std::fs::create_dir_all(&services).expect("create the service directory");
        std::fs::write(
            services.join("org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar"),
            PLUGIN_SERVICE,
        )
        .expect("write the registrar service file");
        let runtime = dir.join("runtime");
        let mut arguments = vec!["-d".to_string(), runtime.display().to_string()];
        arguments.extend(write_sources(
            &dir.join("runtime-src"),
            &[("ExpectInMultiplatform.kt", RUNTIME_SOURCE)],
        ));
        compile_with_kotlinc(arguments, "multiplatform runtime");
        FleetBuild {
            plugin: jar_directory(&plugin, &dir.join("expects-plugin.jar")),
            runtime,
        }
    })
}

/// Package the plugin's classes and service file as a jar: kotlinc's `-Xplugin` and the test
/// harness's compile records both identify a plugin by its jar.
fn jar_directory(dir: &Path, jar: &Path) -> PathBuf {
    use std::io::Write;
    fn add(archive: &mut zip::ZipWriter<std::fs::File>, base: &Path, dir: &Path) {
        let mut entries = std::fs::read_dir(dir)
            .expect("read a plugin output directory")
            .map(|entry| entry.expect("a plugin output entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                add(archive, base, &entry);
                continue;
            }
            let name = entry
                .strip_prefix(base)
                .expect("an entry under the plugin output")
                .to_string_lossy()
                .replace('\\', "/");
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .expect("start a jar entry");
            archive
                .write_all(&std::fs::read(&entry).expect("read a plugin file"))
                .expect("write a jar entry");
        }
    }
    let mut archive = zip::ZipWriter::new(std::fs::File::create(jar).expect("create the plugin jar"));
    add(&mut archive, dir, dir);
    archive.finish().expect("finish the plugin jar");
    jar.to_path_buf()
}

fn switches() -> Vec<String> {
    vec![format!("-Xplugin={}", fleet_build().plugin.display())]
}

const COMMON: &str = r#"package demo

import fleet.util.multiplatform.linkToActual

internal fun join(a: Int, s: String): String = linkToActual()

fun count(list: List<String>): Int {
    return linkToActual()
}

fun <T : Comparable<T>> first(items: List<T>, fallback: T): T = linkToActual()

fun String.shout(times: Int): String = linkToActual()

fun touch(): Unit = linkToActual()

fun withDefault(n: Int = 3): Int = linkToActual()

fun <K, V> makeMap(): MutableMap<K, V> = linkToActual()

fun total(vararg values: Int): Int = linkToActual()

fun apply(value: Int, transform: (Int) -> Int): Int = linkToActual()

context(prefix: String)
fun label(n: Int): String = linkToActual()

suspend fun pause(block: () -> Any?): Any? = linkToActual()

fun box(): String {
    if (join(1, "a") != "a1") return "join"
    if (count(listOf("x", "y")) != 2) return "count"
    val head: String = first(listOf("b", "a"), "z")
    if (head != "b") return "first"
    if ("hi".shout(2) != "hihi") return "shout"
    touch()
    if (withDefault() != 3) return "default"
    val map = makeMap<String, Int>()
    map["a"] = 1
    if (map != mapOf("a" to 1)) return "map"
    if (total(1, 2, 3) != 6) return "vararg"
    if (apply(2) { it * 5 } != 10) return "function"
    return "OK"
}
"#;

const ACTUALS: &str = r#"package demo

import fleet.util.multiplatform.Actual

@Actual
internal fun joinJvm(a: Int, s: String): String = s + a

@Actual
fun countJvm(list: List<String>): Int = list.size

@Actual
fun <E : Comparable<E>> firstJvm(items: List<E>, fallback: E): E = items.firstOrNull() ?: fallback

@Actual
fun String.shoutJvm(times: Int): String = repeat(times)

@Actual
fun touchJvm() {}

@Actual
fun withDefaultJvm(n: Int): Int = n

@Actual("makeMap")
internal fun <A, B> makeMapJvm(): MutableMap<A, B> = HashMap()

@Actual
fun totalJvm(vararg values: Int): Int = values.sum()

@Actual
fun applyJvm(value: Int, transform: (Int) -> Int): Int = transform(value)

@Actual
context(prefix: String)
fun labelJvm(n: Int): String = prefix + n

@Actual
suspend fun pauseJvm(block: () -> Any?): Any? = block()
"#;

/// Expression and block bodies, an internal pair, generic functions whose actual names its type
/// parameters differently, an extension, `Unit`, a default argument, a vararg, a function-typed
/// parameter, a context parameter and a suspend function.
#[test]
fn linked_functions_call_their_actuals_like_kotlinc() {
    let build = fleet_build();
    let fixture = PluginFixture::new("fleet-expects");
    let sources = [("Common.kt", COMMON), ("Jvm.kt", ACTUALS)];
    let classpath = [build.runtime.clone()];
    let switches = switches();
    let reference = fixture.kotlinc("main", &sources, &classpath, &switches);
    let krusty = fixture.krusty("main", &sources, &classpath, &switches);
    assert_same_classes_and_box(
        &reference,
        &krusty,
        &[common::stdlib_jar(), build.runtime.clone()],
    );
}

const ERRORS: &str = r#"package demo

import fleet.util.multiplatform.Actual
import fleet.util.multiplatform.linkToActual

fun missing(a: Int): String = linkToActual()

fun mismatch(a: Int): String = linkToActual()

fun <T> generic(items: List<T>, f: (T) -> String?, vararg xs: Int): Map<String, T> = linkToActual()

fun String.ext(n: Int): Int = linkToActual()

/** A KDoc and modifiers precede the reported `fun`. */
internal fun ret(): String = linkToActual()

fun <T : Any> bound(t: T): T = linkToActual()

class Holder {
    fun member(): Int = linkToActual()

    @Actual
    fun helperJvm(): Int = 0

    @Actual
    fun misnamed(): Int = 0
}

@Actual
fun mismatchJvm(a: Long): String = ""

@Actual
fun mismatchJvm(a: Int, b: Int): String = ""

@Actual
internal fun <T> genericJvm(items: List<T>, f: (T) -> String, vararg xs: Int): Map<String, T> = mapOf()

@Actual
fun extJvm(s: String, n: Int): Int = 0

@Actual fun retJvm(): String? = null

@Actual fun <T> boundJvm(t: T): T = t

@Actual
fun badName(): String = ""

@Actual
fun orphanJvm(): String = ""

fun plain(a: Int): Int = a

@Actual
@Suppress("UNUSED_PARAMETER")
fun plainJvm(a: String): Int = 0
"#;

/// `Errors.kt:line:column: error: message` lines of a compiler's output, without the directory the
/// compiler spelled the file with, in the order the compiler printed them.
fn error_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.find("Errors.kt:").map(|start| &line[start..]))
        .filter(|line| line.contains(": error: "))
        .map(str::to_string)
        .collect()
}

/// Every error the plugin reports — a function with no actual, actuals of another signature
/// (parameter count, parameter types, receiver kind, return type, type-parameter bounds), a member
/// on either side, an `@Actual` function without the suffix, and one nothing links to — at the
/// same `fun` keyword, with the same message, in the same order as kotlinc.
#[test]
fn convention_errors_match_kotlinc() {
    let build = fleet_build();
    let dir = common::scratch_dir()
        .expect("a scratch directory")
        .join("fleet-expects-errors");
    let _ = std::fs::remove_dir_all(&dir);
    let source = dir.join("src");
    let files = write_sources(&source, &[("Errors.kt", ERRORS)]);
    let classpath = build.runtime.display().to_string();
    let mut arguments = switches();
    arguments.extend([
        "-cp".to_string(),
        classpath.clone(),
        "-d".to_string(),
        dir.join("kotlinc").display().to_string(),
    ]);
    arguments.extend(files.iter().cloned());
    let (code, reference) =
        common::kotlinc_server::kotlinc_compile_unrecorded(&arguments).expect("the reference compiler runs");
    assert_ne!(code, 0, "kotlinc accepted the invalid declarations");
    let krusty = Command::new(common::krusty_binary())
        .args(switches())
        .args(["-cp", &classpath, "-d"])
        .arg(dir.join("krusty"))
        .args(&files)
        .output()
        .expect("run krusty");
    assert_eq!(krusty.status.code(), Some(1), "krusty rejects the declarations");
    let expected = error_lines(&reference);
    assert_eq!(expected.len(), 18, "kotlinc's errors: {reference}");
    assert_eq!(
        error_lines(&String::from_utf8_lossy(&krusty.stderr)),
        expected
    );
}
