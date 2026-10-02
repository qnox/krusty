//! A local delegated property's accessor plan carries the declaration identity the checker
//! recorded, while every read/write points to that semantic plan. Which declaration a reference
//! names is the checker's decision (`fir::body_check::delegate_tests`); these tests show lowering
//! keeps that identity without copying a reference into each access.

use super::tests::{lower_single_source, lower_source_from_set};
use crate::fir::LocalDelegatedPropertyId;
use crate::ir::{for_each_child, IrExpr, IrFile};

const DELEGATE: &str = "class Delegate(val value: String) {
    operator fun getValue(owner: Any?, property: Any?): String = value
}
";

fn plan_reference(ir: &IrFile, plan: u32) -> (String, LocalDelegatedPropertyId) {
    let plan = &ir.local_delegate_plans[plan as usize];
    let mut pending = vec![plan.getter.body];
    let mut reference = None;
    while let Some(expression) = pending.pop() {
        if let IrExpr::LocalPropertyReference(value) = ir.expr(expression) {
            assert!(reference.is_none(), "one reference per accessor template");
            reference = Some((value.name.to_string(), value.declaration));
        }
        for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    reference.expect("local delegate getter reference")
}

fn plan_references(ir: &IrFile) -> Vec<(String, LocalDelegatedPropertyId)> {
    (0..ir.local_delegate_plans.len())
        .map(|plan| {
            let plan = u32::try_from(plan).expect("local delegate plan id fits u32");
            plan_reference(ir, plan)
        })
        .collect()
}

/// Each function with local delegated-property accesses: its name and every semantic plan id used
/// by its body.
fn accesses_by_function(ir: &IrFile) -> Vec<(String, Vec<u32>)> {
    ir.functions
        .iter()
        .filter_map(|function| {
            let mut pending = vec![function.body?];
            let mut accesses = Vec::new();
            while let Some(expression) = pending.pop() {
                if let IrExpr::LocalDelegateAccess(access) = ir.expr(expression) {
                    accesses.push(access.plan);
                }
                // A lambda's inline template is a copy of its implementation body. That copy is
                // the lambda's read, counted on the lifted function, not a second read of the
                // function that holds the lambda expression.
                if let IrExpr::Lambda { captures, .. } = ir.expr(expression) {
                    captures.iter().for_each(|&capture| pending.push(capture));
                    continue;
                }
                for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
            }
            (!accesses.is_empty()).then(|| (function.name.clone(), accesses))
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
    let references = plan_references(&ir);
    let [(first_name, first), (second_name, second)] = &references[..] else {
        panic!("two references, found {references:?}")
    };
    assert_eq!((first_name.as_str(), second_name.as_str()), ("x", "x"));
    assert_ne!(first, second);
    let accesses = accesses_by_function(&ir);
    let [(function, plans)] = &accesses[..] else {
        panic!("one function reads local delegates, found {accesses:?}")
    };
    assert_eq!(function, "sibling");
    assert_eq!(plans.len(), 2);
    assert!(plans.contains(&0) && plans.contains(&1));
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
    let references = plan_references(&ir);
    let [(name, declaration)] = &references[..] else {
        panic!("one semantic accessor-plan reference, found {references:?}")
    };
    assert_eq!(name, "x");
    assert_eq!(declaration.ordinal(), 0);
    let functions = accesses_by_function(&ir);
    assert_eq!(
        functions
            .iter()
            .find(|(name, _)| name == "captured")
            .map(|(_, plans)| plans.as_slice()),
        Some(&[0][..]),
        "the source function reads the checked declaration"
    );
    let lifted = functions
        .iter()
        .filter(|(name, _)| name != "captured")
        .collect::<Vec<_>>();
    let [(_, plans)] = lifted[..] else {
        panic!("one lifted lambda must read the checked declaration, found {functions:?}")
    };
    assert_eq!(plans, &[0]);
}

#[test]
fn a_delegate_declared_in_a_lambda_does_not_replace_its_enclosing_plan() {
    let ir = lower_single_source(
        "class Token
        class TokenDelegate(val value: Token) {
            operator fun getValue(owner: Any?, property: Any?): Token = value
        }
        fun invokeToken(block: () -> Token): Token = block()
        fun chooseToken(left: Token, right: Token): Token = left
        fun nested(): Token {
            val outer by TokenDelegate(Token())
            val read = {
                val inner by TokenDelegate(Token())
                chooseToken(inner, outer)
            }
            return chooseToken(invokeToken(read), outer)
        }
        ",
        "LocalDelegates",
    );
    let references = plan_references(&ir);
    let [(outer_name, outer), (inner_name, inner)] = &references[..] else {
        panic!("two semantic accessor-plan references, found {references:?}")
    };
    assert_eq!(
        (outer_name.as_str(), inner_name.as_str()),
        ("outer", "inner")
    );
    assert_eq!((outer.ordinal(), inner.ordinal()), (0, 1));
    assert_ne!(outer, inner);
    assert_eq!(ir.local_delegate_plan_ids.get(outer), Some(&0));
    assert_eq!(ir.local_delegate_plan_ids.get(inner), Some(&1));

    let functions = accesses_by_function(&ir);
    assert_eq!(
        functions
            .iter()
            .find(|(name, _)| name == "nested")
            .map(|(_, plans)| plans.as_slice()),
        Some(&[0][..]),
        "the enclosing function must retain the outer plan"
    );
    let lifted = functions
        .iter()
        .filter(|(name, _)| name != "nested")
        .collect::<Vec<_>>();
    let [(_, plans)] = lifted[..] else {
        panic!("one lifted lambda must read both plans, found {functions:?}")
    };
    assert_eq!(plans.len(), 2);
    assert!(plans.contains(&0));
    assert!(plans.contains(&1));
}

#[test]
fn the_same_local_name_in_two_files_is_two_properties() {
    let first =
        format!("{DELEGATE}fun from_first(): String {{ val x by Delegate(\"a\"); return x }}\n");
    let second = "fun from_second(): String { val x by Delegate(\"b\"); return x }\n";
    let sources = [(first.as_str(), "First"), (second, "Second")];
    let left = plan_references(&lower_source_from_set(&sources, 0));
    let right = plan_references(&lower_source_from_set(&sources, 1));
    let [(_, left_declaration)] = &left[..] else {
        panic!("one plan in the first file, found {left:?}")
    };
    let [(_, right_declaration)] = &right[..] else {
        panic!("one plan in the second file, found {right:?}")
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
    let references = plan_references(&ir);
    let [(name, declaration)] = &references[..] else {
        panic!("one checked accessor plan, found {references:?}")
    };
    assert_eq!(name, "x");
    let functions = accesses_by_function(&ir);
    let plans = |name: &str| {
        functions
            .iter()
            .find(|(function, _)| function == name)
            .map(|(_, plans)| plans.as_slice())
            .unwrap_or_else(|| panic!("{name} must carry an inlined local delegate access"))
    };
    assert_eq!(ir.local_delegate_plans.len(), 1);
    assert_eq!(plans("first"), &[0]);
    assert_eq!(plans("second"), &[0]);
    assert_eq!(plan_reference(&ir, 0), (name.clone(), *declaration));
    assert_eq!(declaration.ordinal(), 0);
}

#[test]
fn generic_inline_copies_keep_one_declaration_plan_across_substitutions() {
    let ir = lower_single_source(
        "import kotlin.reflect.KProperty
        interface Root
        class Token : Root
        class Carrier<T : Root>(private val value: T) {
            operator fun getValue(owner: Any?, property: KProperty<*>): T = value
        }
        inline fun <reified T : Root> delegated(value: T): T {
            val local by Carrier(value)
            return local
        }
        fun exact(value: Token): Root = delegated<Token>(value)
        fun erased(value: Root): Root = delegated<Root>(value)
        ",
        "GenericInlineLocalDelegate",
    );

    assert_eq!(ir.local_delegate_plans.len(), 1);
    let functions = accesses_by_function(&ir);
    let plans = |name: &str| {
        functions
            .iter()
            .find(|(function, _)| function == name)
            .map(|(_, plans)| plans.as_slice())
            .unwrap_or_else(|| panic!("{name} must carry an inlined local delegate access"))
    };
    assert_eq!(plans("exact"), &[0]);
    assert_eq!(plans("erased"), &[0]);
}

#[test]
fn nested_delegate_plans_retain_the_containing_checked_inline_declaration() {
    let ir = lower_single_source(
        &format!(
            "{DELEGATE}
            inline fun nested(): String {{
                val first = {{ val firstValue by Delegate(\"a\"); firstValue }}
                val second = {{ val secondValue by Delegate(\"b\"); secondValue }}
                return first() + second()
            }}
            inline val propertyValue: String
                get() {{ val accessorValue by Delegate(\"c\"); return accessorValue }}
            fun ordinary(): String {{
                fun localReader(): String {{
                    val ordinaryValue by Delegate(\"d\")
                    return ordinaryValue
                }}
                return localReader()
            }}"
        ),
        "NestedInlineDeclarationProvenance",
    );
    let plans = &ir.local_delegate_plans;
    assert_eq!(plans.len(), 4);
    let declarations = plan_references(&ir)
        .into_iter()
        .zip(plans)
        .map(|((name, _), plan)| {
            (
                name,
                plan.inline_declaration.map(|inline| inline.declaration),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        declarations.len(),
        4,
        "one declaration record for each distinct fixture property"
    );
    let declaration = declarations["firstValue"].expect("checked inline function");
    assert_eq!(declarations["secondValue"], Some(declaration));
    let accessor = declarations["accessorValue"].expect("checked inline accessor");
    assert_ne!(accessor, declaration);
    assert_eq!(declarations["ordinaryValue"], None);
    let lambdas = plan_references(&ir)
        .into_iter()
        .zip(plans)
        .map(|((name, _), plan)| (name, plan.declaration_lambda))
        .collect::<std::collections::HashMap<_, _>>();
    let first = lambdas["firstValue"].expect("first exact source lambda");
    let second = lambdas["secondValue"].expect("second exact source lambda");
    assert_ne!(first, second);
    assert_eq!(lambdas["accessorValue"], None);
    assert_eq!(lambdas["ordinaryValue"], None);
}

#[test]
fn delegate_declaration_lambda_is_inherited_through_a_local_function() {
    let ir = lower_single_source(
        &format!(
            "{DELEGATE}
        inline fun declaration(): String = {{
            val direct by Delegate(\"a\")
            fun localRead(): String {{ val nested by Delegate(\"b\"); return nested }}
            direct + localRead()
        }}.invoke()"
        ),
        "DelegateDeclarationLambdaIdentity",
    );
    assert_eq!(ir.local_delegate_plans.len(), 2);
    let first = &ir.local_delegate_plans[0];
    let second = &ir.local_delegate_plans[1];
    assert!(first.declaration_lambda.is_some());
    assert_eq!(first.declaration_lambda, second.declaration_lambda);
    assert_eq!(first.inline_declaration, second.inline_declaration);
}
