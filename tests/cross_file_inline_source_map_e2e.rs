//! Source map of a same-module `inline fun` expanded from another file of the compilation.
//!
//! kotlinc names the callee's own file (and its facade, or its classifier for a member) and keeps
//! the caller's identity range at the caller's line count. A longer callee file must not stretch
//! that range, and a same-file expansion stays on file 1.
use std::fs;

use super::common;

const LIB: &str = "\
package sample

inline fun widen(value: Int): Int {
    val doubled = value + value




    return doubled
}
";

const MAIN: &str = "\
package sample

fun box(): Int = widen(21)
";

const HOLDER: &str = "\
package sample

class Holder {
    inline fun tag(value: Int): Int {
        val labeled = value + 1
        return labeled
    }
}
";

const USE: &str = "\
package sample

fun box(holder: Holder): Int = holder.tag(21)
";

const SAME_FILE: &str = "\
package sample

inline fun widen(value: Int): Int {
    val doubled = value + value
    return doubled
}

fun box(): Int = widen(21)
";

/// Both compilers' `SourceDebugExtension` for `class`, compiled from `sources` in one module.
fn source_maps(
    tag: &str,
    sources: &[(&str, &str)],
    class: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let _jh = common::java_home();
    let dir = std::env::temp_dir().join(format!("krusty_smap_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let reference_dir = dir.join("ref");
    fs::create_dir_all(&reference_dir).unwrap();
    let mut arguments = vec![
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
    ];
    for (name, text) in sources {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        arguments.push(path.to_string_lossy().into_owned());
    }
    let (code, stderr) = common::kotlinc_compile(&arguments)?;
    assert_eq!(code, 0, "{tag}: kotlinc failed: {stderr}");

    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    let classes = common::compile_in_process_files(sources, &classpath, None)
        .unwrap_or_else(|| panic!("{tag}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{tag}: krusty did not emit {class}"));
    let krusty_class = dir.join("out.class");
    fs::write(&krusty_class, bytes).unwrap();
    let reference_class = reference_dir.join(format!("{class}.class"));
    let both = (
        common::source_debug_extension(&reference_class),
        common::source_debug_extension(&krusty_class),
    );
    let _ = fs::remove_dir_all(&dir);
    Some(both)
}

fn assert_map(tag: &str, sources: &[(&str, &str)], class: &str, expected: &[&str]) {
    let Some((reference, krusty)) = source_maps(tag, sources, class) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    assert_eq!(
        reference, expected,
        "{tag}: the reference map, spelled out so a change on either side is visible here"
    );
    assert_eq!(krusty, reference, "{tag}: SourceDebugExtension");
}

#[test]
fn a_top_level_inline_from_another_file_keeps_the_caller_range() {
    assert_map(
        "widen",
        &[("Lib.kt", LIB), ("Main.kt", MAIN)],
        "sample/MainKt",
        &[
            "SMAP",
            "Main.kt",
            "Kotlin",
            "*S Kotlin",
            "*F",
            "+ 1 Main.kt",
            "sample/MainKt",
            "+ 2 Lib.kt",
            "sample/LibKt",
            "*L",
            "1#1,4:1",
            "4#2,6:5",
            "*S KotlinDebug",
            "*F",
            "+ 1 Main.kt",
            "sample/MainKt",
            "*L",
            "3#1:5,6",
            "*E",
        ],
    );
}

#[test]
fn a_member_inline_from_another_file_is_named_by_its_class() {
    assert_map(
        "tag",
        &[("Holder.kt", HOLDER), ("Use.kt", USE)],
        "sample/UseKt",
        &[
            "SMAP",
            "Use.kt",
            "Kotlin",
            "*S Kotlin",
            "*F",
            "+ 1 Use.kt",
            "sample/UseKt",
            "+ 2 Holder.kt",
            "sample/Holder",
            "*L",
            "1#1,4:1",
            "5#2,2:5",
            "*S KotlinDebug",
            "*F",
            "+ 1 Use.kt",
            "sample/UseKt",
            "*L",
            "3#1:5,2",
            "*E",
        ],
    );
}

#[test]
fn a_same_file_inline_stays_on_the_caller_file() {
    assert_map(
        "same",
        &[("Main.kt", SAME_FILE)],
        "sample/MainKt",
        &[
            "SMAP",
            "Main.kt",
            "Kotlin",
            "*S Kotlin",
            "*F",
            "+ 1 Main.kt",
            "sample/MainKt",
            "*L",
            "1#1,9:1",
            "4#1,2:10",
            "*S KotlinDebug",
            "*F",
            "+ 1 Main.kt",
            "sample/MainKt",
            "*L",
            "8#1:10,2",
            "*E",
        ],
    );
}
