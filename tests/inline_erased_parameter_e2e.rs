//! An inlined type parameter is stored as its erased bound.
//!
//! `T : AutoCloseable?` occupies an `AutoCloseable` slot. `value?.close()` invokes that bound
//! directly. `T : Comparable<T>` is stored as `Comparable` and `checkcast` back to `String` only
//! at `length`. An unconstrained `T` stays the specialized reference, so `String.length` has no
//! cast. The complete classpath consumer class is compared with kotlinc; the larger generic `use`
//! expansion and its same-file debug surface belong to `inline_use_close_e2e`.

use super::common;

const SOURCE: &str = r#"inline fun <T : java.lang.AutoCloseable?> closeNow(value: T) {
    value?.close()
}

inline fun <T : Comparable<T>> id(value: T): T = value
inline fun <T> plainId(value: T): T = value

fun close(reader: java.io.BufferedReader?) = closeNow(reader)
fun narrow(value: String) = id(value).length
fun keep(value: String): Comparable<String> = id(value)
fun plain(value: String) = plainId(value).length
"#;

const LIBRARY: &str = r#"package erased.bound

inline fun <T : java.lang.AutoCloseable?> closeNow(value: T) {
    value?.close()
}

inline fun <T : Comparable<T>> id(value: T): T = value
"#;

const CONSUMER: &str = r#"package erased.bound

fun close(reader: java.io.BufferedReader?) = closeNow(reader)
fun keep(value: String): Comparable<String> = id(value)
"#;

#[test]
fn an_inlined_type_parameter_is_stored_as_its_erased_bound() {
    let library = common::kotlinc_lib_out(&[("ErasedBoundLibrary.kt", LIBRARY)])
        .expect("reference kotlinc compiles the inline dependency");
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "InlineErasedConsumer",
        CONSUMER,
        &["erased/bound/InlineErasedConsumerKt"],
        &[library],
    );
}

#[test]
fn an_inlined_bound_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val reader = java.io.BufferedReader(java.io.StringReader(\"ab\"))\n\
             \x20   close(reader)\n\
             \x20   close(null)\n\
             \x20   if (plain(\"ok\") != 2) return \"plain\"\n\
             \x20   if (narrow(\"abcd\") != 4) return \"narrow\"\n\
             \x20   return if (keep(\"x\") == \"x\") \"OK\" else \"keep\"\n\
             }}\n"
        ),
        "inlined erased bound",
    );
}
