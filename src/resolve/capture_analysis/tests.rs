use super::anonymous_class_uses_name;
use crate::ast::Decl;
use crate::diag::DiagSink;
use crate::resolve::capture_storage::ConstructorArgumentUses;

fn uses_outer_value(outer_type: &str, members: &str, extension_calls: bool) -> bool {
    let source = format!("fun enclosing(value: {outer_type}) = object {{\n{members}\n}}");
    let mut diagnostics = DiagSink::new();
    let tokens = crate::lexer::lex(&source, &mut diagnostics);
    let file = crate::parser::parse(&source, &tokens, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert_eq!(file.anonymous_object_classes.len(), 1, "{source}");
    let declaration = *file.anonymous_object_classes.values().next().unwrap();
    let Decl::Class(class) = file.decl(declaration) else {
        panic!("anonymous construction must own a class declaration");
    };
    assert!(class.base_args.is_empty(), "{source}");
    anonymous_class_uses_name(
        &file,
        class,
        &[],
        "value",
        extension_calls,
        ConstructorArgumentUses::Candidates,
    )
}

#[test]
fn anonymous_member_parameters_and_default_prefixes_keep_lexical_scope() {
    for (members, expected) in [
        ("fun read(value: Int): Int = value", false),
        ("fun read(value: Int = value): Int = value", true),
        ("fun read(value: Int, next: Int = value): Int = next", false),
        (
            "fun read(value: Int): Int = value; fun sibling(): Int = value",
            true,
        ),
    ] {
        assert_eq!(
            uses_outer_value("Int", members, false),
            expected,
            "{members}"
        );
    }
}

#[test]
fn qualified_receiver_function_uses_follow_the_same_parameter_scope() {
    let shadowed = "fun read(value: Int.() -> Int): Int = 1.value()";
    let sibling = "fun read(value: Int.() -> Int): Int = 1.value(); fun sibling(): Int = 2.value()";
    assert!(!uses_outer_value("Int.() -> Int", shadowed, true));
    assert!(uses_outer_value("Int.() -> Int", sibling, true));
    // A member spelling alone is not evidence that the enclosing value is an extension callable.
    assert!(!uses_outer_value("Int.() -> Int", sibling, false));
}

#[test]
fn anonymous_lambda_it_binding_uses_selected_value_parameter_shape() {
    use crate::ast::{Expr, ExprId};
    use crate::fx_hash::HashMap;
    use crate::resolve::{ExprLowering, LambdaInfo};
    use crate::types::Ty;

    for (lambda_source, declared_empty) in [
        ("{ it }", false),
        ("{ -> it }", true),
        ("fun(): Int = it", true),
    ] {
        let source = format!("fun enclosing(it: Int) = object {{ val read = {lambda_source} }}");
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(&source, &mut diagnostics);
        let file = crate::parser::parse(&source, &tokens, &mut diagnostics);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert_eq!(file.anonymous_object_classes.len(), 1);
        let declaration = *file.anonymous_object_classes.values().next().unwrap();
        let Decl::Class(class) = file.decl(declaration) else {
            unreachable!()
        };
        let lambdas = file
            .expr_arena
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                matches!(node, Expr::Lambda { .. }).then_some(ExprId(index as u32))
            })
            .collect::<Vec<_>>();
        assert_eq!(lambdas.len(), 1);
        let lambda = lambdas[0];
        let mut expressions = HashMap::default();
        expressions.insert(lambda, ExprLowering::Lambda(LambdaInfo::default()));
        let delegations = HashMap::default();
        assert!(anonymous_class_uses_name(
            &file,
            class,
            &[],
            "it",
            false,
            ConstructorArgumentUses::Candidates
        ));
        for (signature, value_arity) in [
            (Ty::fun(vec![], Ty::Int), 0),
            (Ty::fun(vec![Ty::Int], Ty::Int), 1),
            (
                Ty::fun_with_shape(vec![Ty::String], Ty::Int, 0, true, false),
                0,
            ),
            (
                Ty::fun_with_shape(vec![Ty::String, Ty::Int], Ty::Int, 1, false, false),
                1,
            ),
        ] {
            let mut types = vec![Ty::Error; file.expr_arena.len()];
            types[lambda.0 as usize] = signature;
            let uses = ConstructorArgumentUses::Checked {
                expressions: &expressions,
                types: &types,
                delegations: &delegations,
            };
            assert_eq!(
                anonymous_class_uses_name(&file, class, &[], "it", false, uses),
                declared_empty || value_arity == 0,
                "{lambda_source}: {signature:?}"
            );
        }
    }
}
