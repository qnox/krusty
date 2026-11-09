//! A same-module inline call whose callee's body creates an object of an anonymous class is
//! inlined from the callee's compiled body, as kotlinc compiles it: the caller reuses the
//! callee's object when nothing specializes it, and regenerates it per call site when a reified
//! type argument does. A private inline function's object is outside the public ABI, and its copy
//! takes the call site's scope. An object a regenerated copy's constructor creates is regenerated
//! with it, as the copy's first object.

use super::common;

// PLATFORM-SPECIFIC: exact JVM class inventories, bytes, metadata and reference runtime errors.
// Whole portable inputs are also selected by the canonical backend/version inventory.
fn with_same_module_sources<T>(run: impl FnOnce(&[(&str, &str)]) -> T) -> T {
    let (blocks, java) = krusty::conformance::split_files(include_str!(
        "box_regressions/inlineCompiledBodyObjects.kt"
    ));
    assert!(java.is_empty());
    let files: Vec<_> = blocks
        .into_iter()
        .map(|(stem, source)| (format!("{stem}.kt"), source))
        .collect();
    let files: Vec<_> = files
        .iter()
        .map(|(name, source)| (name.as_str(), source.as_str()))
        .collect();
    run(&files)
}

#[test]
fn an_inline_bodys_objects_are_inlined_from_the_compiled_body_like_kotlinc() {
    let classes = with_same_module_sources(common::classes_against_kotlinc_module);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for name in classes.reference.keys() {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn an_inline_bodys_objects_inlined_from_the_compiled_body_run() {
    with_same_module_sources(|files| common::expect_box_ok_files_with_stdlib(files, "CallerKt"));
}

const NESTED: &str = include_str!("box_regressions/inlineCompiledNestedObjects.kt");

#[test]
fn an_object_a_regenerated_constructor_creates_is_regenerated_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Nested.kt", NESTED)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for name in classes.reference.keys() {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn an_object_a_regenerated_constructor_creates_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Nested.kt", NESTED)], "NestedKt");
}

/// PLATFORM-SPECIFIC: this deliberately preserves kotlinc/JVM's exact NoSuchFieldError.
/// kotlinc declares a copy's fields before it regenerates the constructor's objects, so a field of
/// an anonymous type keeps the original's type while the constructor stores the copy: the program
/// fails as kotlinc's does.
const UNTYPED_NESTED: &str = r#"inline fun <reified T> untyped(): Any = object {
    val type = T::class
    val nested = object {}
}

fun box(): String = if (untyped<Int>() is Any) "OK" else "untyped"
"#;

#[test]
fn an_object_of_anonymous_type_a_regenerated_constructor_creates_fails_as_kotlinc_does() {
    let reference = common::kotlinc_box_result(UNTYPED_NESTED);
    assert_eq!(
        reference,
        "ERROR:NoSuchFieldError:Class MainKt$box$$inlined$untyped$1 does not have member field \
         'MainKt$box$$inlined$untyped$1$1 nested'"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(UNTYPED_NESTED, "Main"),
        reference
    );
}

/// A regenerated object's suspend method constructs its continuation, which is regenerated with
/// the object; the copy's method names the copied continuation from its first instruction.
const SUSPEND_METHOD: &str = include_str!("box_regressions/inlineCompiledSuspendObjects.kt");

#[test]
fn a_regenerated_objects_suspend_method_takes_a_regenerated_continuation_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Suspending.kt", SUSPEND_METHOD)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    // A continuation declared in an inline function's scope still carries krusty's protected
    // synthetic-class visibility in its metadata, where kotlinc records a local, public-ABI one.
    for name in [
        "Named",
        "SuspendingKt",
        "SuspendingKt$box$1",
        "SuspendingKt$box$1$invokeSuspend$$inlined$make$1",
        "SuspendingKt$make$1",
    ] {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_regenerated_objects_suspend_method_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Suspending.kt", SUSPEND_METHOD)], "SuspendingKt");
}

/// An object whose supertype names a reified parameter (`object : Inv<T>()`) carries it in its
/// generic signature, so each call site regenerates the object with the signature specialized.
const REIFIED_SUPERTYPE: &str =
    include_str!("box_regressions/inlineCompiledReifiedSupertypeObjects.kt");

#[test]
fn an_object_whose_supertype_names_a_reified_parameter_is_regenerated_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Reified.kt", REIFIED_SUPERTYPE)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for name in classes.reference.keys() {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn an_object_whose_supertype_names_a_reified_parameter_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Reified.kt", REIFIED_SUPERTYPE)], "ReifiedKt");
}
