//! An inline function's anonymous object overrides an interface property with a narrower type, the
//! type of another anonymous object. Each call site copies the outer object, and the copy's getter
//! keeps the narrower result, so the copy needs the interface's getter bridge as the declaration
//! does.
//!
//! kotlinc 2.4 writes classes for this program that throw `AbstractMethodError` (KT-20245), so the
//! shared box regression is bounded to kotlinc 2.5 and later, where it runs on every backend against
//! kotlinc. This also pins krusty's own JVM run on every release.

use krusty::kotlin_version::{Quirk, Quirks};

use super::common;

const PROGRAM: &str = include_str!("box_regressions/InlineObjectPropertyBridge.kt");

#[test]
fn an_inline_object_copy_bridges_its_narrowed_property_getter() {
    // kotlinc before 2.5 declares the copy's `value` field with the original nested class's type
    // ([`Quirk::InlineObjectFieldKeepsOriginalNestedType`]), which krusty reproduces by default.
    let expected = if Quirks::of_release(krusty::kotlin_version::environment_release()).has(Quirk::InlineObjectFieldKeepsOriginalNestedType) {
        "ERROR:NoSuchFieldError:Class MainKt$box$$inlined$wrap$1 does not have member field \
         'MainKt$box$$inlined$wrap$1$1 value'"
    } else {
        "OK"
    };
    assert_eq!(
        common::expect_box_run_with_stdlib_diverging(PROGRAM, "Main", "OK"),
        expected
    );
}
