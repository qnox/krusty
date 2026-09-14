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

/// Newest locally cached jar whose file name starts with `prefix`.
///
/// The shared `find_jar` searches only `~/.m2/repository/org/jetbrains` beside `~/.gradle` and
/// prefers the SHORTEST name, which does not locate the `-jvm` serialization artifacts; a lookup
/// that silently returns `None` here would turn every test in this file into a silent pass.
fn newest_jar(prefix: &str) -> Option<PathBuf> {
    fn walk(dir: &std::path::Path, prefix: &str, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 9 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, prefix, depth + 1, out);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(prefix) && name.ends_with(".jar") && !name.contains("sources")
                })
            {
                out.push(path);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let mut found = Vec::new();
    for root in [format!("{home}/.gradle"), format!("{home}/.m2")] {
        walk(std::path::Path::new(&root), prefix, 0, &mut found);
    }
    found.sort();
    found.pop()
}

/// kotlinx-serialization runtime, or `None` when it is not cached locally.
fn serialization_jars() -> Option<Vec<PathBuf>> {
    static JARS: OnceLock<Option<Vec<PathBuf>>> = OnceLock::new();
    JARS.get_or_init(|| {
        Some(vec![
            common::stdlib_jar(),
            newest_jar("kotlinx-serialization-core-jvm")?,
            newest_jar("kotlinx-serialization-json-jvm")?,
        ])
    })
    .clone()
}

/// A missing runtime must not make these tests silently pass: report the skip loudly, exactly once.
fn runtime_or_skip(test: &str) -> Option<Vec<PathBuf>> {
    let jars = serialization_jars();
    if jars.is_none() {
        eprintln!("SKIPPING {test}: kotlinx-serialization runtime jars are not cached locally");
    }
    jars
}

/// Build the dependency with the REFERENCE compiler and its serialization plugin, then hand back the
/// output directory. A real dependency presents class files and `@Metadata` produced by kotlinc, and
/// the behaviour under test is how krusty READS one — so the dependency must not come from krusty.
fn dependency_dir(tag: &str, source: &str, cp: &[PathBuf]) -> Option<PathBuf> {
    let work = common::scratch_dir()?.join(tag);
    std::fs::create_dir_all(&work).ok()?;
    let lib = work.join("Lib.kt");
    std::fs::write(&lib, source).ok()?;
    let out = work.join("classes");
    let plugin = common::kotlinc_lib_dir()?.join("kotlinx-serialization-compiler-plugin.jar");
    if !plugin.is_file() {
        return None;
    }
    let mut args = vec![
        format!("-Xplugin={}", plugin.display()),
        "-jvm-target".to_string(),
        "25".to_string(),
        "-d".to_string(),
        out.display().to_string(),
    ];
    if !cp.is_empty() {
        args.push("-cp".to_string());
        args.push(
            std::env::join_paths(cp)
                .ok()?
                .to_string_lossy()
                .into_owned(),
        );
    }
    args.push(lib.display().to_string());
    let (code, stderr) = common::kotlinc_compile(&args)?;
    assert_eq!(
        code, 0,
        "{tag}: kotlinc could not build the dependency: {stderr}"
    );
    Some(out)
}

const LIB: &str = "package dep\n\
import kotlinx.serialization.Serializable\n\
\n\
@Serializable\n\
data class InfraConfig(val name: String, val size: Int = 1)\n";

/// The failing shape: both directions against a type that exists only on the classpath.
#[test]
fn a_classpath_serializable_type_round_trips_through_the_reified_helpers() {
    let Some(runtime) =
        runtime_or_skip("a_classpath_serializable_type_round_trips_through_the_reified_helpers")
    else {
        return;
    };
    let Some(dep) = dependency_dir("classpath_serializable_dep", LIB, &runtime) else {
        eprintln!("SKIPPING: the reference serialization plugin is not provisioned");
        return;
    };
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
    let Some(runtime) = runtime_or_skip("same_file_and_sibling_file_declarations_still_round_trip")
    else {
        return;
    };

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
    let Some(runtime) = runtime_or_skip("the_driver_accepts_a_classpath_serializable_round_trip")
    else {
        return;
    };
    let Some(dep) = dependency_dir("classpath_serializable_dep", LIB, &runtime) else {
        eprintln!("SKIPPING: the reference serialization plugin is not provisioned");
        return;
    };
    let mut cp = vec![dep];
    cp.extend(runtime);

    const MAIN: &str = "import dep.InfraConfig\n\
import kotlinx.serialization.json.Json\n\
\n\
fun read(json: Json, text: String): InfraConfig = json.decodeFromString<InfraConfig>(text)\n\
fun write(json: Json, value: InfraConfig): String = json.encodeToString(value)\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &cp);
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
}
