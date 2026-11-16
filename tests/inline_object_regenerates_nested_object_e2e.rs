//! An object nested in an inline function's anonymous object captures the function's
//! `crossinline` lambda through the outer object's constructor. Each call regenerates both
//! objects: the nested copy is a class of the outer copy (`{copy}$1`) with the lambda spliced into
//! it, and every body of the outer copy, including a method copied before the nested object was
//! regenerated, uses the nested copy. kotlinc before 2.5 miscompiles the program (KT-20245), so the
//! default oracle cannot run it; the 2.5 conformance lanes compare it with kotlinc.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/inlineObjectRegeneratesNestedObject.kt");

#[test]
fn a_nested_object_is_regenerated_under_the_inline_object_copy() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "InlineObjectRegeneratesNestedObject"),
        "OK"
    );
}

#[test]
fn the_regenerated_classes_take_kotlinc_names() {
    let mut names = common::expect_compile_in_process(
        PROGRAM,
        "InlineObjectRegeneratesNestedObject",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .into_iter()
    .map(|(name, _)| name)
    .filter(|name| name.contains("$$inlined$"))
    .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            "InlineObjectRegeneratesNestedObjectKt$box$$inlined$wrap$1",
            "InlineObjectRegeneratesNestedObjectKt$box$$inlined$wrap$1$1",
            "InlineObjectRegeneratesNestedObjectKt$box$$inlined$wrap$2",
            "InlineObjectRegeneratesNestedObjectKt$box$$inlined$wrap$2$1",
        ]
    );
}
