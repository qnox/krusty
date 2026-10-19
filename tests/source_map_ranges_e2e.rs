//! How the `SourceDebugExtension` (SMAP) names and ranges the lines of an expanded `inline fun`.
//!
//! The reference compiler maps a dependency's lines one at a time, in the order the inlined code
//! carries them. A line joins an existing range of its file when that range came from the same call
//! and covers it — or, for the range at the frontier of the output space, lies at most ten lines
//! past its end — and otherwise opens a new range. A line the dependency itself got by inlining is
//! first read back through the DEPENDENCY's own map, so it is named by the file it really came from,
//! under the class that holds it.
//!
//! krusty reserved one region spanning the lowest to the highest line and named it by the facade a
//! call goes through:
//!
//! ```text
//! kotlinc:  + 2 _Collections.kt                         krusty:  + 2 _Collections.kt
//!           kotlin/collections/CollectionsKt___CollectionsKt       kotlin/collections/CollectionsKt
//!           1739#2:3                                               1739#2,2367:3
//!           1814#2,3:4
//! ```
//!
//! Every class that expands a stdlib collection function carried that difference.
use std::fs;
use std::path::PathBuf;

use super::common;

/// Both compilers' maps of `class` compiled from `main` against `classpath`, one entry per line, or
/// `None` when the reference toolchain is unavailable.
fn source_maps(
    tag: &str,
    main: &str,
    class: &str,
    classpath: &[PathBuf],
) -> Option<(Vec<String>, Vec<String>)> {
    let _jh = common::java_home(); // panics with the JAVA_HOME diagnosis when absent
    let dir = std::env::temp_dir().join(format!("krusty_smap_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    fs::create_dir_all(&reference_dir).unwrap();
    fs::create_dir_all(&krusty_dir).unwrap();
    let source = dir.join("Main.kt");
    fs::write(&source, main).unwrap();
    let mut arguments = Vec::new();
    if !classpath.is_empty() {
        arguments.push("-classpath".to_string());
        arguments.push(
            std::env::join_paths(classpath)
                .expect("join the classpath")
                .to_string_lossy()
                .into_owned(),
        );
    }
    arguments.extend([
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ]);
    let (code, stderr) = common::kotlinc_compile(&arguments)?;
    assert_eq!(code, 0, "{tag}: kotlinc failed: {stderr}");

    let mut krusty_classpath = classpath.to_vec();
    krusty_classpath.extend([common::stdlib_jar(), common::jdk_modules()]);
    let classes = common::compile_in_process_metadata_cp(main, "Main", &krusty_classpath)
        .unwrap_or_else(|| panic!("{tag}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{tag}: krusty did not emit {class}"));
    fs::write(krusty_dir.join(format!("{class}.class")), bytes).unwrap();

    let both = (
        common::source_debug_extension(&reference_dir.join(format!("{class}.class"))),
        common::source_debug_extension(&krusty_dir.join(format!("{class}.class"))),
    );
    let _ = fs::remove_dir_all(&dir);
    Some(both)
}

/// The `*F` section of a map, as `(file name, path)` in declaration order.
fn mapped_files(map: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = map.iter().skip_while(|line| line.as_str() != "*F").skip(1);
    while let Some(line) = lines.next() {
        let Some((_, name)) = line
            .strip_prefix("+ ")
            .and_then(|rest| rest.split_once(' '))
        else {
            break;
        };
        let Some(path) = lines.next() else {
            break;
        };
        out.push((name.to_string(), path.clone()));
    }
    out
}

/// A dependency written for this test, so every line of the expected map is known here rather
/// than read off whichever stdlib the toolchain carries. `spaced`'s body marks lines 1 and 2, then
/// line 22 — twenty lines past the range lines 1..=2 opened, further than the frontier range may
/// stretch, so the map carries two ranges rather than one covering 22 lines.
const LIBRARY: &str = "inline fun spaced(f: () -> Int): Int {\n\
    \x20   val first = f()\n\
    \n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\
    \x20   return first + f()\n\
    }\n";

#[test]
fn lines_far_apart_open_two_ranges() {
    let Some(library) = common::compile_lib_ref("smap_spaced", LIBRARY) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    const MAIN: &str = "fun g(): Int = spaced { 21 }\n";
    let Some((reference, krusty)) = source_maps("spaced", MAIN, "MainKt", &[library]) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        reference,
        vec![
            "SMAP",
            "Main.kt",
            "Kotlin",
            "*S Kotlin",
            "*F",
            "+ 1 Main.kt",
            "MainKt",
            "+ 2 Lib.kt",
            "LibKt",
            "*L",
            "1#1,2:1",
            "2#2:3",
            "22#2:4",
            "*S KotlinDebug",
            "*F",
            "+ 1 Main.kt",
            "MainKt",
            "*L",
            "1#1:3",
            "1#1:4",
            "*E",
        ],
        "the reference map, spelled out so a change on either side is visible here"
    );
    assert_eq!(krusty, reference, "SourceDebugExtension");
}

/// A stdlib collection function: its body is read from the multifile PART class, and part of it was
/// itself inlined there from another function, so its lines resolve through the part class's own
/// map. The exact line numbers belong to the stdlib the toolchain carries, so they are compared
/// rather than pinned; the file table and the shape are pinned.
#[test]
fn a_stdlib_function_is_named_by_its_part_class() {
    const MAIN: &str = "fun f(xs: List<Int>): List<String> = xs.map { it.toString() }\n";
    let Some((reference, krusty)) = source_maps("stdlib_map", MAIN, "MainKt", &[]) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        mapped_files(&reference),
        vec![
            ("Main.kt".to_string(), "MainKt".to_string()),
            (
                "_Collections.kt".to_string(),
                "kotlin/collections/CollectionsKt___CollectionsKt".to_string()
            ),
        ],
        "the reference names the part class that holds the body"
    );
    assert_eq!(krusty, reference, "SourceDebugExtension");
}

/// Two expansions from two different lines: each opens its own ranges, and the debug stratum sends
/// each back to its own call.
#[test]
fn two_calls_on_two_lines_keep_their_own_ranges() {
    const MAIN: &str = "fun f(xs: List<Int>): List<String> {\n\
        \x20   val evens = xs.filter { it % 2 == 0 }\n\
        \x20   return evens.map { it.toString() }\n\
        }\n";
    let Some((reference, krusty)) = source_maps("two_calls", MAIN, "MainKt", &[]) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        mapped_files(&reference),
        vec![
            ("Main.kt".to_string(), "MainKt".to_string()),
            (
                "_Collections.kt".to_string(),
                "kotlin/collections/CollectionsKt___CollectionsKt".to_string()
            ),
        ]
    );
    assert_eq!(krusty, reference, "SourceDebugExtension");
}

/// A same-file `suspend inline` function expanded by two methods. kotlinc numbers each method's
/// inlined lines against a map of that method's own, so both expansions take lines 16 onward
/// there — which is what each continuation's `@DebugMetadata` records — and only writing the
/// methods copies them into the class map, the second after the first. A synthetic line inside an
/// expansion belongs to the call that expanded it, so each method's takes a row of its own.
const PER_METHOD: &str = "class Sess : AutoCloseable { override fun close() {} suspend fun start() {} suspend fun add(k: String): String = k }\n\
    class Mgr { suspend fun acquire(o: String): Sess = Sess() }\n\
    class T(val mgr: Mgr, val org: String) {\n\
    \x20   suspend fun a(x: String?): String = withAuth(x) { it + \"a\" }\n\
    \x20   suspend fun b(x: String?): String = withAuth(x) { it + \"b\" }\n\
    \x20   private suspend inline fun <R> withAuth(x: String?, crossinline block: suspend (String) -> R): R {\n\
    \x20       val s = x ?: return block(\"none\")\n\
    \x20       return mgr.acquire(org).use { sess ->\n\
    \x20           sess.start()\n\
    \x20           val k = (s as? String)?.let { sess.add(it) }\n\
    \x20           block(s + k)\n\
    \x20       }\n\
    \x20   }\n\
    }\n";

#[test]
fn each_method_numbers_its_inlined_lines_on_its_own() {
    let Some((reference, krusty)) = source_maps("per_method", PER_METHOD, "T", &[]) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    let rows: Vec<&str> = reference
        .iter()
        .skip_while(|line| line.as_str() != "*L")
        .map(String::as_str)
        .collect();
    assert_eq!(
        rows,
        vec![
            "*L",
            "1#1,15:1",
            "7#1,4:16",
            "11#1:21",
            "7#1,4:22",
            "11#1:27",
            "1#2:20",
            "1#2:26",
            "1#2:28",
            "*S KotlinDebug",
            "*F",
            "+ 1 Main.kt",
            "T",
            "*L",
            "4#1:16,4",
            "4#1:21",
            "5#1:22,4",
            "5#1:27",
            "4#1:20",
            "5#1:26",
            "*E",
        ],
        "the reference map, spelled out so a change on either side is visible here"
    );
    assert_eq!(krusty, reference, "SourceDebugExtension");
    for continuation in ["T$a$1", "T$b$1"] {
        common::class_bytes_diff_against_kotlinc(
            "PerMethod",
            &[],
            PER_METHOD,
            continuation,
            "the whole continuation class",
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|difference| panic!("{difference}"));
    }
}
