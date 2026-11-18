//! A `crossinline` lambda that a classpath inline function's anonymous object invokes while it is
//! constructed, from a property initializer or an `init` block, is inlined into the object's
//! constructor at the call. The original constructor reads the lambda from its parameter, as
//! kotlinc's does; the copy's constructor takes the lambda's captured values in its place and
//! holds the lambda's body where the invoke was.

use super::common;

const LIB: &str = r#"
package lib

inline fun counted(crossinline text: () -> String) =
    object {
        val value = text()
        var count = 0

        init {
            count = value.length
        }

        fun grow(): Int {
            count += 1
            return count
        }

        override fun toString() = value + grow()
    }

inline fun both(crossinline text: () -> String) =
    object {
        val first = text()

        override fun toString() = first + text()
    }
"#;

const MAIN: &str = r#"
import lib.*

fun counting(prefix: String) = counted { prefix + "!" }

fun twice(word: String) = both { word }

fun box(): String {
    val result = counting("O").toString() + counting("KK").toString() + twice("a").toString()
    return if (result == "O!3KK!4aa") "OK" else result
}
"#;

#[test]
fn a_lambda_read_during_construction_runs_like_the_reference_compiler() {
    let output = common::expect_box_run_against_ref("crossinline_object_initializer", LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The copies are kotlinc's, byte for byte: the constructor holds each lambda's body where the
/// original read the parameter, and the lambda's captured values are the copy's fields.
#[test]
fn the_object_copies_are_kotlincs() {
    let classes = common::classes_against_kotlinc_lib("Initializing", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "InitializingKt",
            "InitializingKt$counting$$inlined$counted$1",
            "InitializingKt$twice$$inlined$both$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
