use super::*;

struct EmptySource;
impl crate::symbol_source::SymbolSource for EmptySource {}

#[test]
fn captured_formals_do_not_make_a_fixed_constructor_generic() {
    let owner = type_name("test/Outer$Inner");
    let mut declaration = crate::libraries::LibraryType::declaration_header();
    declaration.is_kotlin = true;
    declaration.is_nested = true;
    declaration.outer_instance = Some(type_name("test/Outer"));
    for own_type_parameter_count in [0, 1] {
        let formals = if own_type_parameter_count == 0 {
            vec!["T".to_string()]
        } else {
            vec!["U".to_string(), "T".to_string()]
        };
        declaration.type_parameters = crate::types::TypeParameters::new(
            formals.clone(),
            vec![vec![Ty::obj("kotlin/Any")]; formals.len()],
            vec![crate::types::TypeVariance::Invariant; formals.len()],
        );
        declaration.own_type_parameter_count = own_type_parameter_count;
        let candidate = CtorDelegationCandidate {
            target: ResolvedCtorDelegationTarget::Super {
                owner,
                params: vec![Ty::Int],
                declaration_params: vec![Ty::Int],
                implicit_outer: Some(type_name("test/Outer")),
                stable_declaration: None,
                external_identity: None,
            },
            context_count: 0,
            param_names: vec!["value".to_string()],
            defaults: vec![false],
            vararg: None,
            supports_default_abi: false,
            low_priority: false,
            parameter_constraints: vec![ConstructorParameterConstraint::Concrete],
            implicit_integer_coercion: vec![false],
            lambda_receivers: vec![false],
        };
        let mismatch = constructor_arguments::ConstructorArgumentMismatch {
            source_argument: 0,
            expected: Ty::Int,
            actual: Ty::obj("kotlin/String"),
        };
        let mut rejection = ConstructorCandidateRejection::new(0);
        rejection.first = Some(mismatch);
        rejection.mapped_mismatches = vec![mismatch];
        rejection.argument_slots = vec![0];
        rejection.declared_arguments = vec![Ty::Int];
        let group = RejectedProviderConstructors::new(
            owner,
            &declaration,
            vec![candidate],
            vec![rejection],
        );
        let mut contenders = group
            .type_mismatches()
            .map(|(_, rejection)| {
                (
                    "constructor",
                    rejection.declared_arguments.clone(),
                    group.generic(),
                )
            })
            .collect::<Vec<_>>();
        contenders.push(("invoke", vec![Ty::Int], false));
        crate::symbol_resolver::retain_most_specific_declarations(
            &EmptySource,
            None,
            &mut contenders,
            |(_, shape, generic)| (None, shape, *generic, None),
        );
        let expected = if own_type_parameter_count == 0 {
            vec![
                ("constructor", vec![Ty::Int], false),
                ("invoke", vec![Ty::Int], false),
            ]
        } else {
            vec![("invoke", vec![Ty::Int], false)]
        };
        assert_eq!(contenders, expected);
    }
}
