//! A lambda's written parameter type relates to the declared parameter contravariantly: for a
//! function type or a functional interface's method it is an upper bound on the call's type
//! variable, whatever variance the interface declares. `choose({ x: Any -> }, "s")` therefore
//! fixes `T` to `String`, as kotlinc does. A type variable inside an invariant argument
//! (`Box<T>` against a written `Box<Any>`) is still an equality.

use super::common;

const ACCEPTED: &str = r#"
fun interface Sink<in T> { fun accept(t: T) }
fun interface Inv<T> { fun accept(t: T) }

fun <T> viaIn(s: Sink<T>, v: T): T = v
fun <T> viaInv(s: Inv<T>, v: T): T = v
fun <T> viaFunction(s: (T) -> Unit, v: T): T = v
fun <T> viaJava(s: java.util.function.Consumer<T>, v: T): T = v
fun <T> only(s: (T) -> Unit): (T) -> Unit = s

fun f() = viaIn({ x: Any -> }, "s")
fun g(): String = f()

fun box(): String {
    val a: String = viaIn({ x: Any -> }, "a")
    val b: String = viaInv({ x: Any -> }, "b")
    val c: String = viaFunction({ x: Any -> }, "c")
    val d: String = viaJava({ x: Any -> }, "d")
    val e: String = viaIn({ x: CharSequence -> }, "e")
    val alone: (Long) -> Unit = only { x: Long -> }
    alone(1L)
    return if (a + b + c + d + e + g() == "abcdes") "OK" else a + b + c + d + e + g()
}
"#;

#[test]
fn a_written_lambda_parameter_type_does_not_widen_the_argument_type() {
    common::assert_accepted_like_kotlinc(ACCEPTED);
    assert_eq!(
        common::expect_box_run_with_stdlib(ACCEPTED, "SamLambdaParameterInference"),
        "OK"
    );
}

const INFERRED: &str = r#"fun interface Sink<in T> { fun accept(t: T) }
fun interface Inv<T> { fun accept(t: T) }
class Box<T>
fun <T> viaIn(s: Sink<T>, v: T): T = v
fun <T> viaInv(s: Inv<T>, v: T): T = v
fun <T> viaFunction(s: (T) -> Unit, v: T): T = v
fun <T> viaJava(s: java.util.function.Consumer<T>, v: T): T = v
fun <T> viaBox(s: (Box<T>) -> Unit, v: T): T = v
fun check() {
    val a = viaIn({ x: Any -> }, "s"); val a1: Int = a
    val b = viaInv({ x: Any -> }, "s"); val b1: Int = b
    val c = viaFunction({ x: Any -> }, "s"); val c1: Int = c
    val d = viaJava({ x: Any -> }, "s"); val d1: Int = d
    val e = viaIn({ x: CharSequence -> }, "s"); val e1: Int = e
    val f = viaBox({ x: Box<Any> -> }, "s"); val f1: Int = f
}
"#;

/// The inferred type of each call, read back through a mismatch kotlinc reports with it.
#[test]
fn the_inferred_type_matches_kotlinc() {
    let sources = [("Main.kt", INFERRED)];
    let reference_args = common::language_directives::kotlinc_args(INFERRED);
    common::assert_errors_match_kotlinc(&sources, &reference_args);
    assert_eq!(
        common::reference_error_ledger(&sources, &reference_args),
        [
            "Main.kt:10:52: initializer type mismatch: expected 'Int', actual 'String'.",
            "Main.kt:11:53: initializer type mismatch: expected 'Int', actual 'String'.",
            "Main.kt:12:58: initializer type mismatch: expected 'Int', actual 'String'.",
            "Main.kt:13:54: initializer type mismatch: expected 'Int', actual 'String'.",
            "Main.kt:14:61: initializer type mismatch: expected 'Int', actual 'String'.",
            "Main.kt:15:58: initializer type mismatch: expected 'Int', actual 'Any'.",
        ]
    );
}
