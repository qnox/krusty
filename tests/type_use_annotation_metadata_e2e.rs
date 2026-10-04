//! `Type.annotation` (`@Metadata` extension field 100) — an annotation written on a TYPE USE.
//!
//! kotlinc records every non-`SOURCE` annotation applied to a type occurrence in that occurrence's
//! metadata `Type`, at any depth of the type tree, in source order. The record also interns the
//! annotation's descriptor in `d2`, so a missing record shows up as a constant pool one entry short
//! even when the bytecode matches.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let result = common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

const PRELUDE: &str = "package app\n\
    \n\
    @Target(AnnotationTarget.TYPE) annotation class Mark\n\
    @Target(AnnotationTarget.TYPE) @Retention(AnnotationRetention.BINARY) annotation class Kept\n\
    @Target(AnnotationTarget.TYPE) @Retention(AnnotationRetention.SOURCE) annotation class Dropped\n\
    class Item\n\
    class Holder<T>\n";

#[test]
fn a_parameter_and_return_type_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         fun take(item: @Mark Item): @Kept Item = item\n"
    );
    assert_identical("TakeUse", &src, "app/TakeUseKt");
}

#[test]
fn a_type_argument_annotation_is_recorded_on_the_argument() {
    let src = format!(
        "{PRELUDE}\n\
         fun hold(holder: Holder<@Mark Item>): Holder<@Kept @Mark Item> = Holder()\n"
    );
    assert_identical("HoldUse", &src, "app/HoldUseKt");
}

#[test]
fn a_source_retained_type_annotation_is_not_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         fun drop(item: @Dropped Item): @Dropped Item = item\n"
    );
    assert_identical("DropUse", &src, "app/DropUseKt");
}

#[test]
fn a_property_type_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         val stored: @Mark Item = Item()\n"
    );
    assert_identical("StoredUse", &src, "app/StoredUseKt");
}

/// The box corpus's `checkExactType` helper: an internal classpath annotation on a type-parameter
/// use, reached through `@Suppress("INVISIBLE_REFERENCE")`.
#[test]
fn a_classpath_type_annotation_on_a_type_parameter_is_recorded() {
    let src = "package app\n\
        \n\
        @Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\", \"UNUSED_PARAMETER\")\n\
        fun <T> exact(value: @kotlin.internal.Exact T) {}\n";
    assert_identical("ExactUse", src, "app/ExactUseKt");
}

#[test]
fn a_nullable_and_a_function_type_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         fun maybe(item: @Mark Item?, action: @Mark (Item) -> Unit): @Kept Holder<Item>? = null\n"
    );
    assert_identical("MaybeUse", &src, "app/MaybeUseKt");
}

#[test]
fn a_supertype_annotation_is_recorded() {
    let src = format!(
        "{PRELUDE}\n\
         open class Base\n\
         class Derived : @Mark Base()\n"
    );
    assert_identical("DerivedUse", &src, "app/Derived");
}
