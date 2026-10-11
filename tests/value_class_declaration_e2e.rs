//! Which `value class` declarations each target accepts, and the exact errors it reports for the
//! rest, against that target's reference compiler.
//!
//! The JVM requires `@JvmInline` on an inline value class; JS, Wasm and Native read a single-field
//! `value class` as inline from the keyword alone. Every target counts the primary constructor's
//! parameters the same way, and reports a wrong count at the parameter list. Each form runs with
//! and without `-XXLanguage:+FullValueClasses`, which makes a multi-field or parameterless class
//! a full value class instead of an error.
//!
//! An annotated declaration is covered on the JVM only: elsewhere `@JvmInline` is an optional
//! expectation, which needs a KLIB library to resolve.

use krusty::compilation_target::CompilationTarget;

use super::common::{
    front_end_diagnostics_located_for, reference_outcome, stdlib_jar, CompilerOutcome,
};

const EVERY_TARGET: [CompilationTarget; 5] = [
    CompilationTarget::Jvm,
    CompilationTarget::Js,
    CompilationTarget::WasmJs,
    CompilationTarget::WasmWasi,
    CompilationTarget::Native,
];

const UNANNOTATED: [&str; 4] = [
    "value class V\n",
    "value class V()\n",
    "value class V(val x: Int)\n",
    "value class V(val x: Int, val y: Int)\n",
];

const ANNOTATED: [&str; 3] = [
    "@JvmInline\nvalue class V()\n",
    "@JvmInline\nvalue class V(val x: Int)\n",
    "@JvmInline\nvalue class V(val x: Int, val y: Int)\n",
];

const FULL_VALUE_CLASSES: &str = "// LANGUAGE: +FullValueClasses\n";

/// Krusty's errors for `source` analyzed for `target`, each a complete block in the reference
/// compilers' shape: `V.kt:line:column: message`, with any further message lines as `| line`.
fn krusty_errors(target: CompilationTarget, source: &str) -> Vec<String> {
    front_end_diagnostics_located_for(target, source, &[stdlib_jar()], None)
        .into_iter()
        .map(|located| {
            let (position, message) = located
                .split_once(": error: ")
                .unwrap_or_else(|| panic!("unexpected located diagnostic: {located}"));
            let mut lines = message.lines();
            let mut block = format!("V.kt:{position}: {}", lines.next().unwrap_or_default());
            for line in lines {
                block.push_str("\n| ");
                block.push_str(line);
            }
            block
        })
        .collect()
}

/// What krusty does with `source` on `target`: it accepts exactly the source sets it reports no
/// error for.
fn krusty_outcome(target: CompilationTarget, source: &str) -> CompilerOutcome {
    let errors = krusty_errors(target, source);
    CompilerOutcome {
        accepted: errors.is_empty(),
        errors,
    }
}

/// Assert krusty accepts or rejects `form` on `target` as its reference compiler does, with the
/// same complete errors in the same order, with and without full value classes.
fn assert_matches_reference(target: CompilationTarget, form: &str) {
    for full_value_classes in [false, true] {
        let (source, args) = if full_value_classes {
            (
                format!("{FULL_VALUE_CLASSES}{form}"),
                vec!["-XXLanguage:+FullValueClasses".to_string()],
            )
        } else {
            (form.to_string(), Vec::new())
        };
        // kotlinc-native alone may be unprovisioned and unrecorded; the `klib-semantics` lane
        // requires it.
        let Some(expected) = reference_outcome(target, &[("V.kt", &source)], &args) else {
            continue;
        };
        assert_eq!(
            krusty_outcome(target, &source),
            expected,
            "{target:?} outcome for:\n{source}"
        );
    }
}

#[test]
fn an_unannotated_value_class_matches_every_reference_compiler() {
    for target in EVERY_TARGET {
        for form in UNANNOTATED {
            assert_matches_reference(target, form);
        }
    }
}

#[test]
fn a_jvm_inline_value_class_matches_kotlinc() {
    for form in ANNOTATED {
        assert_matches_reference(CompilationTarget::Jvm, form);
    }
}

/// The shapes are pinned too, so a target whose reference compiler is not provisioned still
/// checks them: kotlinc 2.4.20's answers, identical on JS, Wasm and Native.
#[test]
fn a_single_field_value_class_needs_jvm_inline_only_on_the_jvm() {
    let source = "value class V(val x: Int)\n";
    assert_eq!(
        krusty_errors(CompilationTarget::Jvm, source),
        vec![
            "V.kt:1:1: value classes without '@JvmInline' annotation are not yet supported."
                .to_string()
        ]
    );
    for target in &EVERY_TARGET[1..] {
        assert_eq!(
            krusty_errors(*target, source),
            Vec::<String>::new(),
            "{target:?}"
        );
    }
}

#[test]
fn a_wrong_parameter_count_is_reported_at_the_parameter_list_on_every_target() {
    for target in EVERY_TARGET {
        assert_eq!(
            krusty_errors(target, "value class V()\n"),
            vec![
                "V.kt:1:14: value class must have exactly one primary constructor parameter."
                    .to_string()
            ],
            "{target:?}"
        );
        assert_eq!(
            krusty_errors(target, "value class V(val x: Int, val y: Int)\n"),
            vec!["V.kt:1:1: the feature \"full value classes\" is experimental and should be enabled explicitly. This can be done by supplying the compiler argument '-XXLanguage:+FullValueClasses', but note that no stability guarantees are provided."
                .to_string()],
            "{target:?}"
        );
    }
    assert_eq!(
        krusty_errors(
            CompilationTarget::Jvm,
            "@JvmInline\nvalue class V(val x: Int, val y: Int)\n"
        ),
        vec![
            "V.kt:2:14: value class must have exactly one primary constructor parameter."
                .to_string()
        ]
    );
}
