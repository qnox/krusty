//! A nested classifier (`enum class`/`class`) is in scope for the WHOLE enclosing class/object body,
//! regardless of declaration order — unlike properties, which obey initialization order. kotlinc
//! accepts a member typed by (or initialized from) a nested classifier declared BELOW it; krusty
//! reported `unresolved reference 'Caption'` because the parser dropped nested `enum class`/`object`/
//! `interface` declarations in an `object` body instead of hoisting them like a class body does.
//! Found on intellij-community's ActionUtil.kt (`object ActionUtil` with a `@JvmField` property typed
//! by the nested `enum class ActionGroupPopupCaption` declared below it).

use super::common;

#[test]
fn object_property_typed_by_nested_enum_declared_below() {
    const SRC: &str = r#"
object Util {
    val c: Caption = Caption.NONE

    enum class Caption { NONE, ALWAYS }
}

fun box(): String = if (Util.c == Util.Caption.NONE) "OK" else "F"
"#;
    common::expect_box_ok_with_stdlib(SRC, "ObjNestedEnumFwd");
}

#[test]
fn object_property_generic_type_arg_references_nested_enum_declared_below() {
    const SRC: &str = r#"
class Key<T> {
    companion object {
        fun <T> create(name: String): Key<T> = Key()
    }
}

object Util {
    val CAPTION: Key<Caption> = Key.create("CAPTION")

    enum class Caption {
        NONE, ALWAYS
    }
}

fun box(): String = if (Util.CAPTION != null) "OK" else "F"
"#;
    common::expect_box_ok_with_stdlib(SRC, "ObjNestedEnumGenericFwd");
}

#[test]
fn object_method_return_type_references_nested_class_declared_below() {
    const SRC: &str = r#"
object Outer {
    fun mk(): Inner = Inner()

    class Inner {
        fun v(): String = "OK"
    }
}

fun box(): String = Outer.mk().v()
"#;
    common::expect_box_ok_with_stdlib(SRC, "ObjNestedClassFwd");
}

#[test]
fn nested_class_references_sibling_nested_class_declared_below_both() {
    const SRC: &str = r#"
object Outer {
    class A {
        fun mk(): B = B()
    }

    class B {
        fun v(): String = "OK"
    }
}

fun box(): String = Outer.A().mk().v()
"#;
    common::expect_box_ok_with_stdlib(SRC, "ObjNestedSiblingFwd");
}

#[test]
fn genuinely_absent_classifier_still_reports_unresolved_reference() {
    const SRC: &str = r#"
object Util {
    val c: NotHere = Caption.NONE

    enum class Caption { NONE, ALWAYS }
}

fun box(): String = "OK"
"#;
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("unresolved reference 'NotHere'")),
        "expected unresolved reference 'NotHere', got: {diagnostics:?}"
    );
}

#[test]
fn class_property_typed_by_nested_enum_declared_below() {
    const SRC: &str = r#"
class Holder {
    val c: Caption = Caption.NONE

    enum class Caption { NONE, ALWAYS }
}

fun box(): String = if (Holder().c == Holder.Caption.NONE) "OK" else "F"
"#;
    common::expect_box_ok_with_stdlib(SRC, "ClassNestedEnumFwd");
}
