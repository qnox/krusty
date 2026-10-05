//! Which accessors a property's `JvmPropertySignature` names, measured against kotlinc 2.4.20. It
//! names the property's own accessor methods only. A private property with default accessors has
//! none, so a member function that merely shares the accessor's JVM name (`getValue`, `setValue`)
//! must not be recorded as its getter or setter.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_private_property_does_not_adopt_a_same_named_member_as_its_getter() {
    let src = "package app\n\
        \n\
        class Delegate<T>(private val value: T) {\n\
        \x20   operator fun getValue(reference: Nothing?, property: kotlin.reflect.KProperty<*>): T = value\n\
        }\n";
    assert_identical("AccessorSignatureGetter", src, "app/Delegate");
}

#[test]
fn a_private_property_does_not_adopt_a_same_named_member_as_its_setter() {
    let src = "package app\n\
        \n\
        class Cell<T>(private var value: T) {\n\
        \x20   operator fun getValue(reference: Nothing?, property: kotlin.reflect.KProperty<*>): T = value\n\
        \x20   operator fun setValue(reference: Nothing?, property: kotlin.reflect.KProperty<*>, next: T) {\n\
        \x20       value = next\n\
        \x20   }\n\
        }\n";
    assert_identical("AccessorSignatureSetter", src, "app/Cell");
}

#[test]
fn a_private_property_does_not_adopt_a_member_with_its_accessor_signature() {
    let src = "package app\n\
        \n\
        class Counter(private var count: Int) {\n\
        \x20   fun getCount(): Int = count\n\
        \x20   fun setCount(next: Int) {\n\
        \x20       count = next\n\
        \x20   }\n\
        }\n";
    assert_identical("AccessorSignatureExact", src, "app/Counter");
}
