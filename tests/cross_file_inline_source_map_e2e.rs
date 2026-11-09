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

const UNIT_LIB: &str = "\
package sample

inline fun announce(value: String?) {
    println(value)
}
";

const CONTRACT_LIB: &str = "\
@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
package sample

import kotlin.contracts.contract

inline fun markNotNull(value: String?) {
    contract { returns() implies (value != null) }
}
";

const UNIT_USE: &str = "\
package sample

fun use(value: String?) {
    announce(value)
    println()
}
";

const CONTRACT_USE: &str = "\
package sample

fun use(value: String?): Int {
    markNotNull(value)
    return value.length
}
";

/// Both compilers' `SourceDebugExtension` for `class`, compiled from `sources` in one module.
fn source_maps(
    tag: &str,
    sources: &[(&str, &str)],
    class: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let (reference, krusty) = class_files(tag, sources, class)?;
    let dir = std::env::temp_dir().join(format!("krusty_smap_{tag}_{}_maps", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let reference_class = dir.join("ref.class");
    let krusty_class = dir.join("out.class");
    fs::write(&reference_class, reference).unwrap();
    fs::write(&krusty_class, krusty).unwrap();
    let both = (
        common::source_debug_extension(&reference_class),
        common::source_debug_extension(&krusty_class),
    );
    let _ = fs::remove_dir_all(&dir);
    Some(both)
}

/// Both compilers' bytes of `class`, compiled from `sources` in one module: kotlinc's first.
fn class_files(tag: &str, sources: &[(&str, &str)], class: &str) -> Option<(Vec<u8>, Vec<u8>)> {
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
    let reference = fs::read(reference_dir.join(format!("{class}.class"))).unwrap();
    let both = (reference, bytes.clone());
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

/// A `Unit` inline function falls off its closing `}`, which kotlinc marks on a `nop` at the call
/// site. That line is the callee's, whichever file of the module declares it.
#[test]
fn a_unit_inline_from_another_file_marks_its_closing_brace() {
    assert_map(
        "announce",
        &[("Lib.kt", UNIT_LIB), ("Use.kt", UNIT_USE)],
        "sample/UseKt",
        &[
            "SMAP",
            "Use.kt",
            "Kotlin",
            "*S Kotlin",
            "*F",
            "+ 1 Use.kt",
            "sample/UseKt",
            "+ 2 Lib.kt",
            "sample/LibKt",
            "*L",
            "1#1,7:1",
            "4#2,2:8",
            "*S KotlinDebug",
            "*F",
            "+ 1 Use.kt",
            "sample/UseKt",
            "*L",
            "4#1:8,2",
            "*E",
        ],
    );
    assert_same_class(
        "announce",
        &[("Lib.kt", UNIT_LIB), ("Use.kt", UNIT_USE)],
        "sample/UseKt",
    );
}

/// An erased `contract { … }` leaves the body empty; the expansion's only line is still the
/// callee's closing `}`.
#[test]
fn an_inline_with_only_a_contract_marks_its_closing_brace() {
    let sources = [("Lib.kt", CONTRACT_LIB), ("Use.kt", CONTRACT_USE)];
    assert_same_class("contract", &sources, "sample/UseKt");
}

fn assert_same_class(tag: &str, sources: &[(&str, &str)], class: &str) {
    let Some((reference, krusty)) = class_files(tag, sources, class) else {
        eprintln!("skip (reference toolchain unavailable)");
        return;
    };
    assert!(
        reference == krusty,
        "{tag}: {class} differs from kotlinc's bytes"
    );
}
