//! `krusty::jvm::class_comparison` names the primary way two classes of the same name differ.
//! Each case compiles two versions of one source with krusty and compares a class between them, so
//! the class files are real compiler output rather than hand-assembled bytes.

use super::common;
use krusty::jvm::class_comparison::{compare_classes, ClassDifference, DifferenceKind};

fn class(src: &str, name: &str) -> Vec<u8> {
    let classes = common::compile_in_process(
        src,
        "Sample",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .unwrap_or_else(|| panic!("krusty compiles:\n{src}"));
    classes
        .into_iter()
        .find(|(internal, _)| internal == name)
        .unwrap_or_else(|| panic!("krusty emits {name}"))
        .1
}

fn difference(reference: &str, candidate: &str, name: &str) -> Option<ClassDifference> {
    compare_classes(&class(reference, name), &class(candidate, name))
}

fn kind_and_member(kind: DifferenceKind, member: &str) -> Option<ClassDifference> {
    Some(ClassDifference {
        kind,
        member: Some(member.to_string()),
    })
}

#[test]
fn identical_classes_have_no_difference() {
    let src = "fun answer(): Int = 42\n";
    assert_eq!(difference(src, src, "SampleKt"), None);
}

#[test]
fn a_method_only_one_class_declares_is_a_member_difference() {
    assert_eq!(
        difference(
            "fun answer(): Int = 42\n",
            "fun answer(): Int = 42\nfun other(): Int = 1\n",
            "SampleKt",
        ),
        kind_and_member(DifferenceKind::Members, "other ()I"),
    );
}

#[test]
fn another_constant_is_an_instruction_difference() {
    assert_eq!(
        difference(
            "fun answer(): Int = 42\n",
            "fun answer(): Int = 41\n",
            "SampleKt"
        ),
        kind_and_member(DifferenceKind::Instructions, "answer ()I"),
    );
}

#[test]
fn another_visibility_is_a_declaration_difference() {
    assert_eq!(
        difference(
            "fun answer(): Int = 42\n",
            "internal fun answer(): Int = 42\n",
            "SampleKt",
        ),
        // `internal` mangles nothing on a top-level function, but it changes the Kotlin metadata
        // only; the JVM declaration stays public.
        Some(ClassDifference {
            kind: DifferenceKind::KotlinMetadata,
            member: None,
        }),
    );
    assert_eq!(
        difference(
            "fun answer(): Int = 42\n",
            "private fun answer(): Int = 42\n",
            "SampleKt",
        ),
        kind_and_member(DifferenceKind::Declarations, "answer ()I"),
    );
}

#[test]
fn a_shifted_body_is_a_line_number_difference_even_though_its_labels_move() {
    assert_eq!(
        difference(
            "fun answer(x: Int): Int {\n    val y = x + 1\n    return y\n}\n",
            "fun answer(x: Int): Int {\n    val y = x + 1\n\n    return y\n}\n",
            "SampleKt",
        ),
        kind_and_member(DifferenceKind::LineNumbers, "answer (I)I"),
    );
}

#[test]
fn a_renamed_local_is_a_local_variable_difference() {
    assert_eq!(
        difference(
            "fun answer(x: Int): Int {\n    val y = x + 1\n    return y\n}\n",
            "fun answer(x: Int): Int {\n    val z = x + 1\n    return z\n}\n",
            "SampleKt",
        ),
        kind_and_member(DifferenceKind::LocalVariables, "answer (I)I"),
    );
}

#[test]
fn reordered_functions_differ_first_in_the_kotlin_metadata_listing_them() {
    // The member order and the line numbers differ too; the metadata, which lists the functions in
    // declaration order, comes first.
    assert_eq!(
        difference(
            "fun a(): Int = 1\nfun b(): Int = 2\n",
            "fun b(): Int = 2\nfun a(): Int = 1\n",
            "SampleKt",
        ),
        Some(ClassDifference {
            kind: DifferenceKind::KotlinMetadata,
            member: None,
        }),
    );
}

#[test]
fn bytes_that_are_no_class_are_unreadable() {
    let class = class("fun answer(): Int = 42\n", "SampleKt");
    assert_eq!(
        compare_classes(&class, b"not a class"),
        Some(ClassDifference {
            kind: DifferenceKind::Unreadable,
            member: None,
        }),
    );
}

#[test]
fn every_kind_round_trips_through_its_identifier() {
    for kind in DifferenceKind::ALL {
        assert_eq!(DifferenceKind::from_id(kind.id()), Some(kind));
    }
}
