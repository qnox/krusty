//! Nested classifiers are in scope throughout their enclosing class or object body, independently of
//! declaration order. These domain-neutral cases pin both forward references and the full lexical
//! identity assigned while class-like declarations are hoisted into the flat file declaration list.
//! No test relies on a particular library, classpath provider, repository, or production class name.

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

#[test]
fn nested_classifiers_under_an_object_keep_the_complete_owner_path() {
    // Parsing `Middle` hoists `Leaf` before `Middle` itself is registered. The shared registration
    // funnel must prefix every declaration created by that child parse, yielding `Root.Middle.Leaf`
    // rather than a truncated `Middle.Leaf` identity.
    const SRC: &str = r#"
object Root {
    class Middle {
        enum class Leaf { VALUE }

        fun value(): Leaf = Leaf.VALUE
    }
}

fun box(): String =
    if (Root.Middle().value() == Root.Middle.Leaf.VALUE) "OK" else "F"
"#;
    common::expect_box_ok_with_stdlib(SRC, "ObjectNestedCompleteOwnerPath");
}

#[test]
fn object_body_classifier_kinds_share_registration_semantics() {
    // Interface and object declarations are class-like AST declarations just as nested classes and
    // enums are. Their owner-qualified identity must not depend on a syntax-specific object-body arm.
    const SRC: &str = r#"
object Scope {
    interface Contract {
        fun value(): String
    }

    object Singleton : Contract {
        override fun value(): String = "OK"
    }
}

fun box(): String = Scope.Singleton.value()
"#;
    common::expect_box_ok_with_stdlib(SRC, "ObjectClassifierRegistrationKinds");
}
