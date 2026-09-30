//! Completion of a postponed producer whose result has no proper call-site constraint.

use super::{GSigBinds, GenericSig, Ty};

/// Complete the declaration-owned variables of an unconstrained producer.
///
/// Kotlin chooses bottom for a result-only variable with no denotable declared constraint. A
/// concrete upper bound remains real evidence; a bound that refers to another variable owned by
/// the same producer is not proper yet and therefore cannot escape into the enclosing inference
/// problem.
pub(crate) fn unconstrained_result_bindings(signature: &GenericSig) -> GSigBinds {
    signature
        .formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            let completion = signature
                .formal_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .filter(|bound| {
                    !signature.formals.iter().any(|owned| {
                        crate::types::ty_mentions_param(*bound, std::slice::from_ref(owned))
                    })
                })
                .unwrap_or(Ty::Nothing);
            (formal.clone(), completion)
        })
        .collect()
}

/// Instantiate a nested result-only call from the concrete parts of an enclosing parameter.
///
/// The parameter may still contain the outer call's inference variables. Those variables are not
/// evidence: completing the nested call to its declared upper bound and feeding that placeholder
/// back would fix the outer variables to the bound. A type that does not mention the outer
/// variables (including a use-site projection of one, such as `in String`) solves the nested
/// variables that occur there. The instantiated return is contributed only when every nested
/// variable that occurs in it was solved that way, so a still-open outer variable such as the `T`
/// in `fun <T> take(x: List<T>)` contributes nothing and the enclosing expectation can still
/// supply it.
///
/// `Ok(None)` means there is no complete concrete solution and ordinary expected-result inference
/// may continue. `Err(())` means concrete evidence conflicted or violated a declaration bound, so
/// the nested candidate is inapplicable and must not fall back.
pub(crate) fn nested_result_from_concrete_parameter(
    nested: &GenericSig,
    parameter: Ty,
    outer_formals: &[String],
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> Result<Option<Ty>, ()> {
    let mut binds = GSigBinds::new();
    if !bind_concrete_parameter(nested, nested.ret, parameter, outer_formals, &mut binds) {
        return Err(());
    }
    let mut solved_a_result_variable = false;
    for formal in &nested.formals {
        if !crate::types::ty_mentions_param(nested.ret, std::slice::from_ref(formal)) {
            continue;
        }
        solved_a_result_variable = true;
        if !binds.contains_key(formal) {
            return Ok(None);
        }
    }
    if !solved_a_result_variable {
        return Ok(None);
    }
    if !super::generic_bindings_satisfy_bounds(nested, &binds, &mut admits) {
        return Err(());
    }
    let instantiated = super::ty_subst_keep_unbound(nested.ret, &binds);
    Ok((!nested
        .formals
        .iter()
        .any(|formal| crate::types::ty_mentions_param(instantiated, std::slice::from_ref(formal))))
    .then_some(instantiated))
}

fn bind_concrete_parameter(
    nested: &GenericSig,
    shape: Ty,
    actual: Ty,
    outer_formals: &[String],
    binds: &mut GSigBinds,
) -> bool {
    let shape = shape.non_null();
    let actual = actual.non_null();
    match shape {
        Ty::StarProjection(_) => true,
        Ty::InProjection(inner) | Ty::OutProjection(inner) => bind_concrete_parameter(
            nested,
            *inner,
            actual.projection_inner().unwrap_or(actual),
            outer_formals,
            binds,
        ),
        Ty::TyParam(name, _) if nested.formals.iter().any(|formal| formal == name) => {
            let constraint = actual.projection_inner().unwrap_or(actual).non_null();
            if !is_concrete_constraint(constraint, &nested.formals, outer_formals) {
                return true;
            }
            match binds.get(name) {
                Some(existing) if *existing != constraint => false,
                Some(_) => true,
                None => {
                    binds.insert(name.to_string(), constraint);
                    true
                }
            }
        }
        Ty::Obj(owner, arguments) => {
            let actual = actual.projection_inner().unwrap_or(actual).non_null();
            let Ty::Obj(actual_owner, actual_arguments) = actual else {
                return true;
            };
            if owner != actual_owner || arguments.len() != actual_arguments.len() {
                return true;
            }
            arguments
                .iter()
                .zip(actual_arguments)
                .all(|(&shape, &actual)| {
                    bind_concrete_parameter(nested, shape, actual, outer_formals, binds)
                })
        }
        _ => true,
    }
}

fn is_concrete_constraint(ty: Ty, nested_formals: &[String], outer_formals: &[String]) -> bool {
    !matches!(ty, Ty::StarProjection(_) | Ty::Error)
        && !crate::types::ty_mentions_param(ty, nested_formals)
        && !crate::types::ty_mentions_param(ty, outer_formals)
}

/// Publish the proper result type of an unconstrained postponed producer without erasing symbolic
/// types owned by an enclosing declaration.
pub(crate) fn instantiate_unconstrained_result(signature: &GenericSig, actual: Ty) -> Ty {
    let live = if signature
        .formals
        .iter()
        .any(|formal| crate::types::ty_mentions_param(actual, std::slice::from_ref(formal)))
    {
        actual
    } else {
        signature.ret
    };
    super::ty_subst_keep_unbound(live, &unconstrained_result_bindings(signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::GenericReturnPolicy;

    fn signature(formals: &[&str], bounds: Vec<Vec<Ty>>, ret: Ty) -> GenericSig {
        GenericSig {
            formals: formals.iter().map(|formal| (*formal).to_string()).collect(),
            formal_bounds: bounds,
            receiver: None,
            params: Vec::new(),
            ret,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn an_unbounded_result_only_variable_completes_to_bottom() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let t = Ty::ty_param("producer:T", any);
        let generic = signature(
            &["producer:T"],
            vec![Vec::new()],
            Ty::obj_args("sample/Set", &[t]),
        );

        assert_eq!(
            instantiate_unconstrained_result(&generic, generic.ret),
            Ty::obj_args("sample/Set", &[Ty::Nothing])
        );
    }

    #[test]
    fn a_concrete_declared_bound_is_the_proper_completion() {
        let t = Ty::ty_param("producer:T", Ty::obj("kotlin/Any"));
        let generic = signature(
            &["producer:T"],
            vec![vec![Ty::obj("kotlin/Any")]],
            Ty::obj_args("sample/Box", &[t]),
        );

        assert_eq!(
            instantiate_unconstrained_result(&generic, generic.ret),
            Ty::obj_args("sample/Box", &[Ty::obj("kotlin/Any")])
        );
    }

    #[test]
    fn completion_does_not_replace_a_caller_owned_symbolic_type() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let producer = Ty::ty_param("producer:T", any);
        let caller = Ty::ty_param("caller:U", any);
        let generic = signature(
            &["producer:T"],
            vec![Vec::new()],
            Ty::obj_args("sample/Pair", &[producer, caller]),
        );

        assert_eq!(
            instantiate_unconstrained_result(&generic, generic.ret),
            Ty::obj_args("sample/Pair", &[Ty::Nothing, caller])
        );
    }

    #[test]
    fn completion_preserves_a_producer_slot_already_fixed_by_an_input() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let a = Ty::ty_param("producer:A", any);
        let b = Ty::ty_param("producer:B", any);
        let generic = signature(
            &["producer:A", "producer:B"],
            vec![Vec::new(), Vec::new()],
            Ty::obj_args("sample/Pair", &[a, b]),
        );
        let actual = Ty::obj_args("sample/Pair", &[Ty::String, b]);

        assert_eq!(
            instantiate_unconstrained_result(&generic, actual),
            Ty::obj_args("sample/Pair", &[Ty::String, Ty::Nothing])
        );
    }

    fn box_of(arguments: &[Ty]) -> Ty {
        Ty::obj_args("sample/Box", arguments)
    }

    #[test]
    fn a_concrete_projection_solves_the_nested_variable_and_open_variables_do_not() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let nested = Ty::ty_param("producer:T", any);
        let generic = signature(
            &["producer:T"],
            vec![vec![any]],
            box_of(&[
                nested,
                Ty::star_projection(any),
                Ty::obj_args("kotlin/collections/List", &[nested]),
            ]),
        );
        let parameter = box_of(&[
            Ty::in_projection(Ty::String),
            Ty::ty_param("gather:A", any),
            Ty::ty_param("gather:R", any),
        ]);

        assert_eq!(
            nested_result_from_concrete_parameter(
                &generic,
                parameter,
                &[
                    "gather:T".to_string(),
                    "gather:A".to_string(),
                    "gather:R".to_string()
                ],
                |actual, bound| actual == bound || bound == any,
            ),
            Ok(Some(box_of(&[
                Ty::String,
                Ty::star_projection(any),
                Ty::obj_args("kotlin/collections/List", &[Ty::String]),
            ])))
        );
    }

    #[test]
    fn an_open_outer_variable_does_not_solve_a_result_only_producer() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let nested = Ty::ty_param("producer:T", any);
        let generic = signature(
            &["producer:T"],
            vec![Vec::new()],
            Ty::obj_args("kotlin/collections/List", &[nested]),
        );
        let parameter = Ty::obj_args("kotlin/collections/List", &[Ty::ty_param("take:T", any)]);

        assert_eq!(
            nested_result_from_concrete_parameter(
                &generic,
                parameter,
                &["take:T".to_string()],
                |actual, bound| actual == bound,
            ),
            Ok(None)
        );
    }

    #[test]
    fn a_concrete_parameter_cannot_violate_the_nested_declaration_bound() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let number = Ty::obj("kotlin/Number");
        let nested = Ty::ty_param("producer:T", number);
        let generic = signature(
            &["producer:T"],
            vec![vec![number]],
            Ty::obj_args("sample/Box", &[nested]),
        );
        let parameter = Ty::obj_args("sample/Box", &[Ty::String]);

        assert_eq!(
            nested_result_from_concrete_parameter(&generic, parameter, &[], |actual, bound| {
                actual == bound || bound == any
            }),
            Err(())
        );
    }
}
