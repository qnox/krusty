//! A reified inline body marks `enumValueOf<T>`, `enumValues<T>` and `enumEntries<T>` with the
//! `ENUM_REIFIED` operation (mode 5) followed by an erased placeholder: `aconst_null` before the
//! name operand and `Enum.valueOf(Class, String)`, `iconst_0; anewarray Enum`, or
//! `aconst_null; checkcast EnumEntries`. Inlining it for a concrete enum rewrites that placeholder
//! into the enum's own `valueOf(String)`, `values()` or `getEntries()`
//! (`ReifiedTypeInliner.processSpecialEnumFunction`). Forwarding the parameter to another reified
//! parameter keeps the placeholder and renames the marker.

use super::common;

const LIB: &str = r#"@file:OptIn(ExperimentalStdlibApi::class)
package lib

import kotlin.enums.enumEntries

inline fun <reified T : Enum<T>> parsed(s: String): T = enumValueOf<T>(s)

inline fun <reified T : Enum<T>> all(): Array<T> = enumValues<T>()

inline fun <reified T : Enum<T>> listed(): List<T> = enumEntries<T>()
"#;

const MAIN: &str = r#"import lib.all
import lib.listed
import lib.parsed

enum class Color { RED, GREEN }

inline fun <reified E : Enum<E>> forwarded(s: String): E = parsed<E>(s)

fun parse(s: String): Color = parsed<Color>(s)

fun values(): Array<Color> = all<Color>()

fun entries(): List<Color> = listed<Color>()

fun box(): String {
    if (parse("GREEN") != Color.GREEN) return "parse"
    if (values().size != 2) return "values"
    if (entries() != listOf(Color.RED, Color.GREEN)) return "entries"
    return "OK"
}
"#;

/// [`MAIN`] calling the forwarding template too: the caller inlines a body that already holds
/// the library's operation under a renamed marker, and rewrites it for its own enum.
const FORWARDING_MAIN: &str = r#"import lib.parsed

enum class Color { RED, GREEN }

inline fun <reified E : Enum<E>> forwarded(s: String): E = parsed<E>(s)

fun box(): String = if (forwarded<Color>("RED") == Color.RED) "OK" else "forward"
"#;

#[test]
fn reified_enum_operations_run_against_a_kotlinc_library() {
    for main in [MAIN, FORWARDING_MAIN] {
        let Some(output) = common::expect_box_run_against_kotlinc(LIB, main) else {
            return;
        };
        assert_eq!(output, "OK");
    }
}

#[test]
fn reified_enum_operations_are_inlined_like_kotlinc() {
    let methods = [
        "public static final Color parse(",
        "public static final Color[] values(",
        "public static final java.util.List<Color> entries(",
        "public static final <E extends java.lang.Enum<E>> E forwarded(",
    ];
    let results = common::class_bytes_diffs_against_kotlinc(
        "ReifiedEnum",
        &[("Lib.kt", LIB)],
        MAIN,
        "ReifiedEnumKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    for (method, result) in methods.into_iter().zip(results) {
        result.unwrap_or_else(|difference| panic!("{method}: {difference}"));
    }
}
