//! How a member property is declared, as `@Metadata` records it: modality (`Property.flags` bits
//! 4-5), `lateinit` (bit 12), delegation (bit 15), and whether each accessor is the compiler default
//! (`isNotDefault`, bit 6 of `getter_flags` / `setter_flags`).
//!
//! Measured against kotlinc 2.4.20: an `override` not marked `final` is OPEN even in a final class,
//! an interface property with a getter is OPEN and one without is ABSTRACT, and a getter source
//! declares, a setter body, a `private set`, or a delegated property's accessor is not the default
//! one, while a bodiless `set` is. An accessor word is written only when it
//! differs from the default word derived from the property. A non-default setter records its value
//! parameter: the written name, `value` for a bodiless `private set`, `<set-?>` for a delegated
//! `var`.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let Some(result) =
        common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
    else {
        eprintln!("skip ({stem}: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn an_open_property_and_its_non_final_override_record_open_modality() {
    const SRC: &str = "package app\n\
        \n\
        open class B {\n\
        \x20   open val p: Int get() = 1\n\
        \x20   open val q: Int = 1\n\
        }\n\
        \n\
        class C : B() {\n\
        \x20   override val p: Int get() = 2\n\
        \x20   override val q: Int = 2\n\
        }\n\
        \n\
        class D : B() {\n\
        \x20   final override val q: Int = 3\n\
        }\n";
    assert_identical("open_property", SRC, "app/B");
    assert_identical("open_property", SRC, "app/C");
    assert_identical("open_property", SRC, "app/D");
}

#[test]
fn an_abstract_class_property_records_abstract_modality() {
    const SRC: &str = "package app\n\
        \n\
        abstract class B {\n\
        \x20   abstract val a: Int\n\
        \x20   open var o: Int = 0\n\
        }\n";
    assert_identical("abstract_property", SRC, "app/B");
}

#[test]
fn an_interface_property_with_a_getter_is_open() {
    const SRC: &str = "package app\n\
        \n\
        interface I {\n\
        \x20   val x: Int get() = 1\n\
        \x20   val y: Int\n\
        }\n";
    assert_identical("interface_property", SRC, "app/I");
}

#[test]
fn a_declared_getter_is_not_the_default_accessor() {
    const SRC: &str = "package app\n\
        \n\
        class A {\n\
        \x20   val x: Int get() = 1\n\
        \x20   var f: Int = 0\n\
        \x20       get() = field\n\
        }\n";
    assert_identical("declared_getter", SRC, "app/A");
}

#[test]
fn a_lateinit_property_records_lateinit() {
    const SRC: &str = "package app\n\
        \n\
        class A {\n\
        \x20   lateinit var s: String\n\
        }\n";
    assert_identical("lateinit_property", SRC, "app/A");
}

#[test]
fn a_delegated_var_records_its_delegate_field_and_setter_parameter() {
    const SRC: &str = "package app\n\
        \n\
        import kotlin.reflect.KProperty\n\
        \n\
        class D {\n\
        \x20   operator fun getValue(t: Any?, p: KProperty<*>): String = \"\"\n\
        \x20   operator fun setValue(t: Any?, p: KProperty<*>, v: String) {}\n\
        }\n\
        \n\
        class A {\n\
        \x20   var x: String by D()\n\
        \x20   val y: String by D()\n\
        }\n";
    assert_identical("delegated_var", SRC, "app/A");
}

#[test]
fn only_a_setter_body_makes_a_declared_setter_not_default() {
    const SRC: &str = "package app\n\
        \n\
        class A {\n\
        \x20   var y: Int = 0\n\
        \x20       set\n\
        \x20   var w: Int = 0\n\
        \x20       set(given) { field = given }\n\
        }\n";
    assert_identical("declared_setter", SRC, "app/A");
}
