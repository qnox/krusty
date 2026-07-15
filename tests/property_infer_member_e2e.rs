//! A property whose type is inferred from a `param.member` initializer (`var c = from.i`, where
//! `from` is a constructor parameter of another user class) now resolves. The collection-phase
//! literal-type inferencer previously resolved a member read only through the classpath, which can't
//! see the module's own classes, so the property's type erased to `Error` and the file was skipped.

use crate::common;

#[test]
fn property_type_from_user_class_member_initializer() {
    let src = r#"
class Box(val i: Int)

class Cursor(val from: Box) {
    var c = from.i
    fun value(): Int = c
}

fun box(): String {
    val cur = Cursor(Box(7))
    return if (cur.value() == 7) "OK" else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(src, "PropInferMember");
}

#[test]
fn property_type_from_chained_member_initializer() {
    let src = r#"
class Inner(val n: Int)
class Outer(val inner: Inner)

class Holder(val o: Outer) {
    val v = o.inner.n
    fun get(): Int = v
}

fun box(): String {
    val h = Holder(Outer(Inner(42)))
    return if (h.get() == 42) "OK" else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(src, "PropInferChainedMember");
}
