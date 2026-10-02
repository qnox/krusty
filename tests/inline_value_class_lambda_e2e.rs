//! A lambda that takes a value class is inlined the way kotlinc inlines it: its body takes the
//! value class unboxed, and the `invoke` it replaces unboxes the `Object` it passes
//! (`StackValue.coerce`: `checkcast` to the box and its `unbox-impl`). That holds for a lambda
//! inlined at the call and for one inlined into an object the inline function creates.

use super::common;

const LIB: &str = r#"
package lib

@JvmInline
value class Tag(val name: String)

@JvmInline
value class Num(val n: Int)

interface Source {
    fun read(value: Any?): String
}

inline fun tagged(crossinline take: (Tag) -> String): Source = object : Source {
    override fun read(value: Any?): String = take(value as Tag)
}

inline fun num(block: (Num) -> Num): Num = block(Num(1))

inline fun <T> reading(value: T, block: (T) -> String): String = block(value)

fun join(first: String, second: String): String = first + second
"#;

const MAIN: &str = r#"
import lib.*

fun suffixed(suffix: String): String = tagged { join(it.name, suffix) }.read(Tag("x"))

fun counted(): Int = num { Num(it.n + 1) }.n

fun generic(): String = reading(Tag("y")) { it.name }

fun box(): String {
    if (suffixed("!") != "x!") return "FAIL suffixed: " + suffixed("!")
    if (counted() != 2) return "FAIL counted: " + counted()
    if (generic() != "y") return "FAIL generic: " + generic()
    return "OK"
}
"#;

#[test]
fn value_class_lambdas_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_kotlinc(LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

#[test]
fn value_class_lambdas_are_the_reference_compilers_classes() {
    let classes =
        common::classes_against_kotlinc_lib("ValueClassLambdas", &[("Lib.kt", LIB)], MAIN)
            .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "ValueClassLambdasKt",
            "ValueClassLambdasKt$suffixed$$inlined$tagged$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A nullable value class over a primitive stays boxed: kotlinc's lambda body takes `Num?` as the
/// box `invoke` passes, so the lambda's node applies no `unbox-impl` coercion to it.
const NULLABLE_LIB: &str = r#"
package lib

@JvmInline
value class Num(val n: Int)

interface Source {
    fun read(value: Any?): Int
}

inline fun maybeCounted(crossinline take: (Num?) -> Int): Source = object : Source {
    override fun read(value: Any?): Int = take(value as Num?)
}
"#;

const NULLABLE_MAIN: &str = r#"
import lib.*

fun absent(): Int = maybeCounted { it?.n ?: -1 }.read(null)

fun present(): Int = maybeCounted { it?.n ?: -1 }.read(Num(7))

fun box(): String {
    if (absent() != -1) return "FAIL absent: " + absent()
    if (present() != 7) return "FAIL present: " + present()
    return "OK"
}
"#;

#[test]
fn nullable_value_class_lambdas_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_kotlinc(NULLABLE_LIB, NULLABLE_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

#[test]
fn nullable_value_class_lambdas_stay_boxed_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib(
        "NullableValueClassLambdas",
        &[("Lib.kt", NULLABLE_LIB)],
        NULLABLE_MAIN,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "NullableValueClassLambdasKt",
            "NullableValueClassLambdasKt$absent$$inlined$maybeCounted$1",
            "NullableValueClassLambdasKt$present$$inlined$maybeCounted$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A value-class parameter forces this lambda to a class. Its two captured implicit receivers keep
/// distinct semantic origins and therefore distinct fields, while kotlinc names both constructor
/// locals `$receiver`.
#[test]
fn a_forced_lambda_class_preserves_each_captured_receiver() {
    let source = r#"
@JvmInline
value class Tag(val value: String)

class Receiver(val name: String)

class Host(val prefix: String) {
    fun Receiver.make(): (Tag) -> String = { prefix + name + it.value }
}
"#;
    common::assert_classes_identical_to_kotlinc("ForcedReceiverLambda", source, &["Host$make$1"]);
}
