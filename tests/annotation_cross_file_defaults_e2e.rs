//! A later file constructs an annotation with declaration defaults that are not literals.
//!
//! The declaring file is checked first and then lowered away. The use file has to carry the
//! closed defaults — enum entries, class literals, arrays, and nested annotation instances —
//! without borrowing that file's expressions.

use super::common;

const DECLARATIONS: &str = r#"
package test

import kotlin.reflect.KClass

enum class E { A, B }

annotation class A()

annotation class B(val a: A = A())

annotation class C(
    val i: Int = 42,
    val b: B = B(),
    val kClass: KClass<*> = B::class,
    val kClassArray: Array<KClass<*>> = [E::class, A::class],
    val e: E = E.B,
    val aS: Array<String> = arrayOf("a", "b"),
    val aI: IntArray = intArrayOf(1, 2)
)

annotation class Partial(
    val i: Int = 42,
    val s: String = "foo",
    val e: E = E.A
)
"#;

const USE: &str = r#"
package test

fun box(): String {
    val c = C()
    if (c.i != 42) return "i"
    if (c.b.a != A()) return "nested"
    if (c.kClass != B::class) return "kclass"
    if (c.kClassArray.size != 2) return "kclass-size"
    if (c.kClassArray[0] != E::class || c.kClassArray[1] != A::class) return "kclass-array"
    if (c.e != E.B) return "enum"
    if (!arrayOf("a", "b").contentEquals(c.aS)) return "strings"
    if (!intArrayOf(1, 2).contentEquals(c.aI)) return "ints"
    val p = Partial(e = E.B, s = "bar")
    if (p.i != 42 || p.s != "bar" || p.e != E.B) return "partial"
    return "OK"
}
"#;

#[test]
fn a_later_file_uses_annotation_constructor_defaults() {
    let sources = [("a.kt", DECLARATIONS), ("b.kt", USE)];
    let compiled = common::compile_and_run_files_with_stdlib(&sources)
        .expect("cross-file annotation defaults");
    assert_eq!(
        compiled,
        common::kotlinc_box_files_result(&sources, "test.BKt")
    );
    assert_eq!(compiled, "OK");
}
