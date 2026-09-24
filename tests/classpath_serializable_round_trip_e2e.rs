//! `decodeFromString<T>` / `encodeToString(v)` where `T` is a CLASSPATH `@Serializable` type.
//!
//! The plugin rewrites these reified helpers into their two-argument form, supplying `T.serializer()`
//! — but it only did so when it could see `T`'s `@Serializable` in the SOURCE being checked. A type
//! that arrives on the classpath carries that annotation in its metadata instead, so the rewrite was
//! skipped, the reified inline call survived to the backend with an empty reified substitution map,
//! and `splice_unified` declined it:
//!
//! ```text
//! [splice] splice_unified reified_inline=true reified_map_len=0
//! error: krusty: JVM backend inline error: inline splice failed
//! ```
//!
//! That is a per-FILE failure, so one such call costs a module every class it would have emitted.
//! The identical type declared in the same file — or in a sibling file of the same module — always
//! worked, which is what kept this narrow: only the classpath spelling lacked the annotation view.
//!
//! STILL UNSUPPORTED, deliberately out of scope here: a type argument that WRAPS the classpath type,
//! `decodeFromString<List<InfraConfig>>`. The plugin keys its rewrite on the type argument's own
//! classifier, and `List` is not `@Serializable`; serving that needs a COMPOSED serializer
//! (`ListSerializer(InfraConfig.serializer())`), which is a separate feature from making the
//! classpath annotation visible.

use std::path::PathBuf;
use std::sync::OnceLock;

use super::common;

fn serialization_core_jar() -> PathBuf {
    krusty::toolchain::serialization_core_jar().unwrap_or_else(|| {
        panic!(
            "no kotlinx-serialization-core-jvm jar; the serialization tests cannot run.\n\
             It is provisioned on demand from Maven Central (version {}); check network access or \
             set KRUSTY_DEPS_CACHE to a directory that already holds it.",
            krusty::toolchain::SERIALIZATION_VERSION
        )
    })
}

fn serialization_json_jar() -> PathBuf {
    krusty::toolchain::serialization_json_jar().unwrap_or_else(|| {
        panic!(
            "no kotlinx-serialization-json-jvm jar; the serialization tests cannot run.\n\
             It is provisioned on demand from Maven Central (version {}); check network access or \
             set KRUSTY_DEPS_CACHE to a directory that already holds it.",
            krusty::toolchain::SERIALIZATION_VERSION
        )
    })
}

/// The pinned kotlinx.serialization runtime, provisioned like every other dependency.
///
/// This used to search the local caches for "the newest jar whose name starts with …" and SKIP the
/// test when it found none. Two things were wrong with that: the search sorted names
/// lexicographically, so `1.9` beat `1.10` and a core jar could be paired with a mismatched json
/// jar; and a skipped serialization test passes on a compiler that would have rejected the fixture.
/// The helpers panic instead, naming what is missing.
fn runtime() -> Vec<PathBuf> {
    static JARS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    JARS.get_or_init(|| {
        vec![
            common::stdlib_jar(),
            serialization_core_jar(),
            serialization_json_jar(),
        ]
    })
    .clone()
}

/// Build the dependency with the REFERENCE compiler and its serialization plugin, then hand back the
/// output directory. A real dependency presents class files and `@Metadata` produced by kotlinc, and
/// the behaviour under test is how krusty READS one — so the dependency must not come from krusty.
/// `-Xplugin=` naming the reference distribution's serialization plugin, the switch a serialization
/// build hands every compiler.
fn serialization_plugin_switch() -> String {
    let plugin = common::kotlinc_lib_dir()
        .expect("no reference compiler lib directory")
        .join("kotlinx-serialization-compiler-plugin.jar");
    assert!(
        plugin.is_file(),
        "the reference serialization plugin is missing at {}",
        plugin.display()
    );
    format!("-Xplugin={}", plugin.display())
}

fn dependency_dir(tag: &str, source: &str, cp: &[PathBuf]) -> PathBuf {
    let work = common::scratch_dir()
        .unwrap_or_else(|| panic!("{tag}: cannot allocate a scratch directory"))
        .join(tag);
    std::fs::create_dir_all(&work).expect("create dependency directory");
    let lib = work.join("Lib.kt");
    std::fs::write(&lib, source).expect("write dependency source");
    let out = work.join("classes");
    let mut args = vec![
        serialization_plugin_switch(),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-d".to_string(),
        out.display().to_string(),
    ];
    if !cp.is_empty() {
        args.push("-cp".to_string());
        args.push(
            std::env::join_paths(cp)
                .expect("build dependency classpath")
                .to_string_lossy()
                .into_owned(),
        );
    }
    args.push(lib.display().to_string());
    let (code, stderr) = common::kotlinc_compile(&args)
        .unwrap_or_else(|| panic!("{tag}: the reference compiler could not be invoked"));
    assert_eq!(
        code, 0,
        "{tag}: kotlinc could not build the dependency: {stderr}"
    );
    out
}

const LIB: &str = "package dep\n\
import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
data class InfraConfig(val name: String, val size: Int = 1)\n";

/// The failing shape: both directions against a type that exists only on the classpath.
#[test]
fn a_classpath_serializable_type_round_trips_through_the_reified_helpers() {
    let runtime = runtime();
    // A tag per test: two tests sharing one scratch directory can truncate and recompile the same
    // `Lib.kt` while the other reads it.
    let dep = dependency_dir("classpath_serializable_reified", LIB, &runtime);
    let mut cp = vec![dep];
    cp.extend(runtime);

    const MAIN: &str = "import dep.InfraConfig\n\
import kotlinx.serialization.json.Json\n\
\n\
fun box(): String {\n\
\x20   val json = Json { ignoreUnknownKeys = true }\n\
\x20   val text = json.encodeToString(InfraConfig(\"a\", 7))\n\
\x20   val back = json.decodeFromString<InfraConfig>(text)\n\
\x20   if (back.name != \"a\") return \"FAIL: name \" + back.name\n\
\x20   if (back.size != 7) return \"FAIL: size \" + back.size\n\
\x20   return \"OK\"\n\
}\n";
    let out = common::compile_and_run_box_files(&[("Main.kt", MAIN)], &cp, None)
        .expect("a classpath @Serializable type must encode and decode");
    assert_eq!(out, "OK");
}

/// The spellings that already worked stay working: the same declaration in the same file, and in a
/// sibling file of the same module.
#[test]
fn same_file_and_sibling_file_declarations_still_round_trip() {
    let runtime = runtime();

    const SAME_FILE: &str = "import kotlinx.serialization.Serializable\n\
import kotlinx.serialization.json.Json\n\
\n\
@Serializable\n\
data class Local(val name: String)\n\
\n\
fun box(): String {\n\
\x20   val json = Json { ignoreUnknownKeys = true }\n\
\x20   val back = json.decodeFromString<Local>(json.encodeToString(Local(\"a\")))\n\
\x20   return if (back.name == \"a\") \"OK\" else \"FAIL: \" + back.name\n\
}\n";
    let out = common::compile_and_run_box_files(&[("Main.kt", SAME_FILE)], &runtime, None)
        .expect("a same-file @Serializable type must round-trip");
    assert_eq!(out, "OK", "same file");

    const SIBLING: &str = "import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
data class Sibling(val name: String)\n";
    const USER: &str = "import kotlinx.serialization.json.Json\n\
\n\
fun box(): String {\n\
\x20   val json = Json { ignoreUnknownKeys = true }\n\
\x20   val back = json.decodeFromString<Sibling>(json.encodeToString(Sibling(\"a\")))\n\
\x20   return if (back.name == \"a\") \"OK\" else \"FAIL: \" + back.name\n\
}\n";
    let out = common::compile_and_run_box_files(
        &[("Sibling.kt", SIBLING), ("Main.kt", USER)],
        &runtime,
        None,
    )
    .expect("a sibling-file @Serializable type must round-trip");
    assert_eq!(out, "OK", "sibling file");
}

/// The compiler DRIVER must accept it too. The in-process helpers above take a different pipeline
/// from a real invocation, so the failure the corpus meets is only observable through the binary.
#[test]
fn the_driver_accepts_a_classpath_serializable_round_trip() {
    let runtime = runtime();
    let dep = dependency_dir("classpath_serializable_driver", LIB, &runtime);
    let mut cp = vec![dep];
    cp.extend(runtime);

    const MAIN: &str = "import dep.InfraConfig\n\
import kotlinx.serialization.json.Json\n\
\n\
fun read(json: Json, text: String): InfraConfig = json.decodeFromString<InfraConfig>(text)\n\
fun write(json: Json, value: InfraConfig): String = json.encodeToString(value)\n";
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Main.kt", MAIN)],
        &cp,
        &[serialization_plugin_switch()],
    );
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected a kotlinc-valid fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    // A zero exit is not enough: this shape used to fail the whole FILE from the backend, and a
    // driver that exits 0 while still rendering a diagnostic would satisfy the status check alone.
    // Assert the complete diagnostic set of BOTH compilers is empty.
    assert_eq!(
        common::compiler_errors(&result.krusty_stdout),
        [],
        "krusty reported errors on stdout: {}",
        result.krusty_stdout
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [],
        "krusty reported errors on stderr: {}",
        result.krusty_stderr
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [],
        "kotlinc reported errors: {}",
        result.reference_stderr
    );
}
