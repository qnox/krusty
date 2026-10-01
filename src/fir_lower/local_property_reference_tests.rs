//! A local delegated property's reflection value carries the declaration identity the checker
//! recorded, in whichever function its read is lowered. Which declaration a reference names is the
//! checker's decision (`fir::body_check::delegate_tests`); these tests show lowering keeps it.

use super::tests::{lower_single_source, lower_source_from_set};
use crate::fir::LocalDelegatedPropertyId;
use crate::ir::{for_each_child, IrExpr, IrFile};

const DELEGATE: &str = "class Delegate(val value: String) {
    operator fun getValue(owner: Any?, property: Any?): String = value
}
";

/// Each function with a local property reference: its name and the `(name, declaration)` of every
/// reference in its body.
fn references_by_function(ir: &IrFile) -> Vec<(String, Vec<(String, LocalDelegatedPropertyId)>)> {
    ir.functions
        .iter()
        .filter_map(|function| {
            let mut pending = vec![function.body?];
            let mut references = Vec::new();
            while let Some(expression) = pending.pop() {
                if let IrExpr::LocalPropertyReference(reference) = ir.expr(expression) {
                    references.push((reference.name.to_string(), reference.declaration));
                }
                for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
            }
            (!references.is_empty()).then(|| (function.name.clone(), references))
        })
        .collect()
}

#[test]
fn same_named_sibling_locals_keep_distinct_identities() {
    let ir = lower_single_source(
        &format!(
            "{DELEGATE}fun sibling(flag: Boolean): String {{
                if (flag) {{ val x by Delegate(\"a\"); return x }}
                else {{ val x by Delegate(\"b\"); return x }}
            }}\n"
        ),
        "LocalDelegates",
    );
    let functions = references_by_function(&ir);
    let [(function, references)] = &functions[..] else {
        panic!("one function reads local delegates, found {functions:?}")
    };
    let [(first_name, first), (second_name, second)] = &references[..] else {
        panic!("two references, found {references:?}")
    };
    assert_eq!(
        (function.as_str(), first_name.as_str(), second_name.as_str()),
        ("sibling", "x", "x")
    );
    assert_ne!(first, second);
}

#[test]
fn a_lambda_and_its_enclosing_function_read_one_declaration() {
    let ir = lower_single_source(
        &format!(
            "{DELEGATE}fun invoke(block: () -> String): String = block()
            fun captured(): String {{
                val x by Delegate(\"c\")
                return invoke {{ x }} + x
            }}\n"
        ),
        "LocalDelegates",
    );
    let functions = references_by_function(&ir);
    let [(enclosing, enclosing_reads), (lambda, lambda_reads)] = &functions[..] else {
        panic!("the function and its lambda read the delegate, found {functions:?}")
    };
    let [(_, declaration), ..] = enclosing_reads[..] else {
        panic!("reads in {enclosing}, found {enclosing_reads:?}")
    };
    let read = ("x".to_owned(), declaration);
    assert_eq!(enclosing.as_str(), "captured");
    assert_ne!(lambda, enclosing);
    // The enclosing body holds its own read and the lambda literal it passes; the lifted lambda
    // holds the literal's read. All of them name the one declaration.
    assert_eq!(enclosing_reads, &[read.clone(), read.clone()]);
    assert_eq!(lambda_reads, &[read]);
}

#[test]
fn the_same_local_name_in_two_files_is_two_properties() {
    let first =
        format!("{DELEGATE}fun from_first(): String {{ val x by Delegate(\"a\"); return x }}\n");
    let second = "fun from_second(): String { val x by Delegate(\"b\"); return x }\n";
    let sources = [(first.as_str(), "First"), (second, "Second")];
    let left = references_by_function(&lower_source_from_set(&sources, 0));
    let right = references_by_function(&lower_source_from_set(&sources, 1));
    let [(_, left_reads)] = &left[..] else {
        panic!("one function in the first file, found {left:?}")
    };
    let [(_, right_reads)] = &right[..] else {
        panic!("one function in the second file, found {right:?}")
    };
    let [(_, left_declaration)] = &left_reads[..] else {
        panic!("one reference in the first file, found {left_reads:?}")
    };
    let [(_, right_declaration)] = &right_reads[..] else {
        panic!("one reference in the second file, found {right_reads:?}")
    };
    assert_ne!(left_declaration, right_declaration);
    assert_ne!(left_declaration.owner(), right_declaration.owner());
    assert_eq!(left_declaration.ordinal(), 0);
    assert_eq!(right_declaration.ordinal(), 0);
}

#[test]
fn two_inline_copies_keep_the_checked_declaration_identity() {
    let ir = lower_single_source(
        &format!(
            "{DELEGATE}inline fun delegated(value: String): String {{
                val x by Delegate(value)
                return x
            }}
            fun first(): String = delegated(\"a\")
            fun second(): String = delegated(\"b\")
            "
        ),
        "InlineLocalDelegate",
    );
    let functions = references_by_function(&ir);
    let references = |name: &str| {
        functions
            .iter()
            .find(|(function, _)| function == name)
            .map(|(_, references)| references.as_slice())
            .unwrap_or_else(|| panic!("{name} must carry an inlined local property reference"))
    };
    let [(first_name, first)] = references("first") else {
        panic!(
            "first must carry exactly one reference, found {:?}",
            references("first")
        )
    };
    let [(second_name, second)] = references("second") else {
        panic!(
            "second must carry exactly one reference, found {:?}",
            references("second")
        )
    };
    assert_eq!((first_name.as_str(), second_name.as_str()), ("x", "x"));
    assert_eq!(first, second);
}
