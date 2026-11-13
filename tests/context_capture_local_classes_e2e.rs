//! A local class or anonymous object whose members pass an enclosing context parameter implicitly.
//!
//! kotlinc captures the binding a member passes as a context argument exactly as it captures a
//! name the member spells: one constructor parameter and field, in first-use order beside the
//! class's other captures, read from the constructor parameter in constructor code. Each class
//! below must be kotlinc's, debug tables included.
use super::common;

const SOURCE: &str = include_str!("box_regressions/ContextCaptureLocalClasses.kt");

// PLATFORM-SPECIFIC: compare every original JVM class byte and execute the shared whole input
// with the original independent artifact and runtime compilation stems.
#[test]
fn local_classes_capture_the_context_parameters_their_members_pass() {
    for class in [
        "ContextCaptureLocalClassesKt",
        "ContextCaptureLocalClassesKt$viaObject$o$1",
        "ContextCaptureLocalClassesKt$viaInterfaceObject$1",
        "ContextCaptureLocalClassesKt$viaLocal$L",
        "ContextCaptureLocalClassesKt$mixed$M",
        "ContextCaptureLocalClassesKt$mixedObject$o$1",
        "ContextCaptureLocalClassesKt$inInit$I",
        "ContextCaptureLocalClassesKt$anonymous$o$1",
        "ContextCaptureLocalClassesKt$shadowed$S",
    ] {
        common::assert_class_matches_kotlinc("ContextCaptureLocalClasses", SOURCE, class);
    }
    common::expect_box_same_as_kotlinc(SOURCE, "ContextCaptureLocalClassesRun");
}

// PLATFORM-SPECIFIC: preserve the complete JVM artifact oracle and official runtime comparison;
// the whole Kotlin program is also selected by the shared portable matrix.
#[test]
fn shadowed_context_captures_keep_the_selected_binding_for_every_class_form() {
    let source = include_str!("box_regressions/ShadowedContextCapture.kt");
    assert_complete_jvm_inventory("ShadowedContextCapture", source);
    common::expect_box_same_as_kotlinc(source, "ShadowedContextCaptureRun");
}

// PLATFORM-SPECIFIC: keep the full JVM artifact and official runtime oracle for the shared
// context-function program, including both local delegated-property accessor bodies.
#[test]
fn context_bound_delegate_dispatch_is_captured_by_the_class_that_uses_it() {
    let source = include_str!("box_regressions/CapturedContextDelegate.kt");
    assert_complete_jvm_inventory("CapturedContextDelegate", source);
    common::expect_box_same_as_kotlinc(source, "CapturedContextDelegateRun");
}

// PLATFORM-SPECIFIC: compare every JVM class and the official runtime for the complete shared
// program; escaped lexical context names do not acquire receiver identity from their spelling.
#[test]
fn escaped_context_name_retains_lexical_storage_with_and_without_a_receiver() {
    let source = include_str!("box_regressions/EscapedNamedContext.kt");
    assert_complete_jvm_inventory("EscapedNamedContext", source);
    common::expect_box_same_as_kotlinc(source, "EscapedNamedContextRun");
}

// PLATFORM-SPECIFIC: the JVM driver compares every emitted class, including local accessors and
// nested lambda classes; the canonical Kotlin programs remain complete portable inputs.
fn assert_complete_jvm_inventory(stem: &str, source: &str) {
    let filename = format!("{stem}.kt");
    let classes = common::classes_against_kotlinc_module(&[(&filename, source)]);
    assert_eq!(
        classes
            .krusty
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        classes
            .reference
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        "complete {stem} class inventory",
    );
    for (name, expected) in &classes.reference {
        assert_eq!(
            &classes.krusty[name], expected,
            "complete {stem} class {name}"
        );
    }
}
