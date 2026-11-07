//! Labeled `super` selects both an enclosing dispatch receiver and one of that receiver's direct
//! supertypes. The checker must preserve both choices for lowering: using the inner class's `this`
//! with the otherwise-correct super accessor/method target is verifier-invalid.

use super::common;

#[test]
fn enclosing_class_labeled_super_property_get_and_set() {
    const SRC: &str = r#"
open class Base(initial: String) {
    open var value: String = initial
}

open class Outer : Base("OK") {
    override var value: String = "override"

    inner class Inner {
        fun result(): String {
            val before = super<Base>@Outer.value
            super<Base>@Outer.value = "DONE"
            return "$before:${super<Base>@Outer.value}"
        }
    }
}

fun box(): String {
    val result = Outer().Inner().result()
    return if (result == "OK:DONE") "OK" else result
}
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_class_labeled_super_interface_default_method() {
    const SRC: &str = r#"
interface Parent {
    fun result(): String = value()
    fun value(): String
}

class Outer : Parent {
    override fun value(): String = "OK"

    inner class Inner : Parent {
        override fun value(): String = "inner"
        fun fromOuterSuper(): String = super<Parent>@Outer.result()
    }
}

fun box(): String = Outer().Inner().fromOuterSuper()
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_class_labeled_super_compound_property_write() {
    const SRC: &str = r#"
open class Base {
    open var value: Int = 500
}

open class Outer : Base() {
    override var value: Int = 200

    inner class Inner {
        fun result(): Int {
            super<Base>@Outer.value += 200
            return super<Base>@Outer.value
        }
    }
}

fun box(): String = if (Outer().Inner().result() == 700) "OK" else "fail"
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_class_typed_labeled_super_inside_lambda() {
    const SRC: &str = r#"
open class Base {
    open var count: Int = 40
    open fun result(): String = "OK"
}

open class Outer : Base() {
    override var count: Int = -1
    override fun result(): String = "override"

    inner class Inner {
        fun run(): String {
            val block = {
                super<Base>@Outer.count += 2
                super<Base>@Outer.result()
            }
            val result = block()
            return if (result == "OK" && super<Base>@Outer.count == 42) "OK" else "fail"
        }
    }
}

fun box(): String = Outer().Inner().run()
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_class_bare_labeled_super_method() {
    const SRC: &str = r#"
open class Base {
    open fun result(): String = "OK"
}

open class Outer : Base() {
    override fun result(): String = "override"

    inner class Inner {
        fun run(): String = super@Outer.result()
    }
}

fun box(): String = Outer().Inner().run()
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_class_labeled_super_bridge_is_not_virtual() {
    const SRC: &str = r#"
open class Base {
    open fun result(): String = "OK"
}

open class Outer : Base() {
    override fun result(): String = "override"

    inner class Inner {
        fun run(): String = super<Base>@Outer.result()
    }
}

class EvilOuter : Outer() {
    fun `access$super`(): String = "HIJACKED"
}

fun box(): String = EvilOuter().Inner().run()
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_class_labeled_super_bridge_preserves_value_class_abi() {
    const SRC: &str = r#"
@JvmInline
value class Value(val text: String)

open class Base {
    open fun echo(value: Value): Value = value
}

open class Outer : Base() {
    override fun echo(value: Value): Value = Value("override")

    inner class Inner {
        fun run(): Value = super<Base>@Outer.echo(Value("OK"))
    }
}

fun box(): String = Outer().Inner().run().text
"#;

    common::expect_box_ok_with_stdlib(SRC, "Main");
}

#[test]
fn enclosing_super_accessor_names_match_the_complete_class_inventory() {
    assert_super_accessor_classes(
        include_str!("fixtures/labeled_super_accessors/BareLabeled.kt"),
        "BareLabeled",
    );
}

#[test]
fn enclosing_super_accessor_names_keep_the_reference_runtime() {
    common::expect_box_same_as_kotlinc(
        include_str!("fixtures/labeled_super_accessors/BareLabeled.kt"),
        "BareLabeled",
    );
}

#[test]
fn enclosing_super_accessor_names_use_the_declared_nested_segment_and_reuse_targets() {
    assert_super_accessor_classes(
        include_str!("fixtures/labeled_super_accessors/PackagedRepeated.kt"),
        "PackagedRepeated",
    );
}

#[test]
fn enclosing_super_accessor_names_preserve_repeated_method_and_property_runtime() {
    let source = include_str!("fixtures/labeled_super_accessors/PackagedRepeated.kt");
    let reference = common::kotlinc_box_files_result(
        &[("PackagedRepeated.kt", source)],
        "bridge.names.PackagedRepeatedKt",
    );
    assert_eq!(reference, "OK", "packaged kotlinc fixture must succeed");
    assert_eq!(
        common::expect_box_run_with_stdlib(source, "PackagedRepeated"),
        reference,
    );
}

fn assert_super_accessor_classes(source: &str, stem: &str) {
    let file = format!("{stem}.kt");
    let classes = common::classes_against_kotlinc_module(&[(&file, source)]);
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let class = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                let header = format!(
                    "{:?}",
                    (
                        class.major,
                        class.access,
                        class.this_class,
                        class.super_class,
                        class.interfaces(),
                        class.signature,
                        class.inner_classes
                    )
                );
                (name.clone(), (header, common::member_table(bytes)))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(inventory(&classes.krusty), inventory(&classes.reference));
}
