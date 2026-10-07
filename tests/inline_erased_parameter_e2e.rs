//! An inlined type parameter is stored as its erased bound.
//!
//! `T : AutoCloseable?` occupies an `AutoCloseable` slot. `this?.close()` invokes that bound
//! directly: a non-null call-site type has no null check and no `checkcast`, and a nullable one
//! keeps `dup; ifnull` on the same slot. `T : Comparable<T>` is stored as `Comparable` and
//! `checkcast` back to `String` only at `length`. An unconstrained `T` stays the specialized
//! reference, so `String.length` has no cast.

use super::common;

const SOURCE: &str = "inline fun <T : java.lang.AutoCloseable?, R> T.use(block: (T) -> R): R {\n\
    \x20   var closed = false\n\
    \x20   try {\n\
    \x20       return block(this)\n\
    \x20   } catch (e: Exception) {\n\
    \x20       closed = true\n\
    \x20       try {\n\
    \x20           this?.close()\n\
    \x20       } catch (closeException: Exception) {\n\
    \x20       }\n\
    \x20       throw e\n\
    \x20   } finally {\n\
    \x20       if (!closed) {\n\
    \x20           this?.close()\n\
    \x20       }\n\
    \x20   }\n\
    }\n\
    \n\
    inline fun <T : Comparable<T>> id(x: T): T = x\n\
    inline fun <T> plainId(x: T): T = x\n\
    \n\
    fun load(reader: java.io.BufferedReader) {\n\
    \x20   reader.use { r -> r.read() }\n\
    }\n\
    \n\
    fun loadNull(reader: java.io.BufferedReader?) {\n\
    \x20   reader.use { }\n\
    }\n\
    \n\
    fun go(s: String) = id(s).length\n\
    fun plain(s: String) = plainId(s).length\n";

const LIBRARY: &str = r#"package erased.bound

inline fun <T : java.lang.AutoCloseable?> closeNow(value: T) {
    value?.close()
}

inline fun <T : Comparable<T>> id(value: T): T = value
inline fun <T> plainId(value: T): T = value
"#;

const CONSUMER: &str = r#"package erased.bound

fun close(reader: java.io.BufferedReader?) = closeNow(reader)
fun narrow(value: String) = id(value).length
fun keep(value: String): Comparable<String> = id(value)
fun plain(value: String) = plainId(value).length
"#;

#[test]
fn an_inlined_type_parameter_is_stored_as_its_erased_bound() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "InlineErasedParameter",
        SOURCE,
        &["InlineErasedParameterKt"],
    );
}

#[test]
fn a_bounded_erasure_stays_erased_until_its_consumer_needs_the_specialization() {
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
             \x20   var n = 0\n\
             \x20   reader.use {{ r -> n = r.read() }}\n\
             \x20   val nullable: java.io.BufferedReader? = java.io.BufferedReader(java.io.StringReader(\"z\"))\n\
             \x20   nullable.use {{ }}\n\
             \x20   val empty: java.io.BufferedReader? = null\n\
             \x20   empty.use {{ }}\n\
             \x20   if (plain(\"ok\") != 2) return \"plain\"\n\
             \x20   return if (n == 'a'.code && go(\"abcd\") == 4) \"OK\" else \"FAIL\"\n\
             }}\n"
        ),
        "inlined erased bound",
    );
}
