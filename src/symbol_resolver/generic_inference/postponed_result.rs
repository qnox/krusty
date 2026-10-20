//! Completion of a postponed producer whose result has no proper call-site constraint.

use super::{GSigBinds, GenericSig, Ty};
use crate::types::TypeName;

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

/// Complete every still-unbound variable whose declared upper bound is the constructed classifier
/// applied to that variable itself.
///
/// Kotlin approximates such a self type with no other constraint by its bound, with the recursive
/// occurrence replaced by a star and the whole argument projected `out`: `Box("x")` for
/// `class Box<SELF : Box<SELF>>` types as `Box<out Box<*>>`, so `withPort(): SELF` reads `Box<*>`
/// and a fluent chain keeps its members. A recursive bound on another classifier
/// (`C<T : Comparable<T>>`) is not a self type; kotlinc cannot infer it and it keeps its existing
/// completion.
fn complete_self_bounded_formals(
    owner: TypeName,
    formals: &[String],
    formal_bounds: &[Vec<Ty>],
    binds: &mut GSigBinds,
) {
    for (formal, bounds) in formals.iter().zip(formal_bounds) {
        let Some(bound) = self_type_bound(owner, formal, formals, bounds) else {
            continue;
        };
        if binds.contains_key(formal) {
            continue;
        }
        // The star carries the bound it reads as, exactly as a written `Box<*>` does.
        let unbounded_star = GSigBinds::from_iter([(
            formal.clone(),
            Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))),
        )]);
        let star = GSigBinds::from_iter([(
            formal.clone(),
            Ty::star_projection(super::ty_subst_keep_unbound(bound, &unbounded_star)),
        )]);
        let completion = Ty::out_projection(super::ty_subst_keep_unbound(bound, &star));
        binds.insert(formal.clone(), completion);
    }
}

/// The declared bound of `formal` when it is a self type of `owner` (`SELF : Box<SELF>` in `Box`).
///
/// The bound names no other formal of `owner`: for `MB<T, SELF : MB<T, SELF>>` kotlinc does not
/// complete `SELF`, even with `T` inferred (`MB("v")` reports `cannot infer type for type
/// parameter 'SELF'`), so that `SELF` keeps its existing completion.
fn self_type_bound(owner: TypeName, formal: &str, formals: &[String], bounds: &[Ty]) -> Option<Ty> {
    let &bound = bounds.first()?;
    let others = formals
        .iter()
        .filter(|other| *other != formal)
        .cloned()
        .collect::<Vec<_>>();
    (bound.non_null().obj_internal() == Some(owner)
        && crate::types::ty_mentions_param(bound, &[formal.to_string()])
        && !crate::types::ty_mentions_param(bound, &others))
    .then_some(bound)
}

/// The classifier arguments of a constructor result once argument and expected-type inference are
/// done. A bound variable keeps its binding, a self type of `owner` completes as
/// [`complete_self_bounded_formals`] describes, and any other takes its first declared bound.
pub(crate) fn completed_constructor_result_arguments(
    owner: TypeName,
    formals: &[String],
    formal_bounds: &[Vec<Ty>],
    bindings: &GSigBinds,
) -> Vec<Ty> {
    let mut completed = bindings.clone();
    complete_self_bounded_formals(owner, formals, formal_bounds, &mut completed);
    formals
        .iter()
        .enumerate()
        .map(|(index, formal)| {
            completed.get(formal).copied().unwrap_or_else(|| {
                formal_bounds
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .filter(|bound| *bound != Ty::Error)
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
            })
        })
        .collect()
}

/// The selected-call type arguments of a constructor of `owner` once argument and expected-type
/// inference are done, in `formals` order. A bound variable keeps its binding and a self type
/// completes as [`complete_self_bounded_formals`] describes; a variable nothing constrained has no
/// solution. Kotlin does not infer it from its declared bound: `C(1)` for
/// `class C<T : Comparable<T>>(val n: Int)` reports `cannot infer type for type parameter 'T'`.
pub(crate) fn solved_constructor_type_arguments(
    owner: TypeName,
    formals: &[String],
    formal_bounds: &[Vec<Ty>],
    bindings: &GSigBinds,
) -> Vec<Option<Ty>> {
    let completed = completed_constructor_result_arguments(owner, formals, formal_bounds, bindings);
    formals
        .iter()
        .zip(
            formal_bounds
                .iter()
                .map(Vec::as_slice)
                .chain(std::iter::repeat(&[][..])),
        )
        .zip(completed)
        .map(|((formal, bounds), completed)| {
            (bindings
                .get(formal)
                .is_some_and(|binding| *binding != Ty::Error)
                || self_type_bound(owner, formal, formals, bounds).is_some())
            .then_some(completed)
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
    let mut binds = GSigBinds::default();
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
            match binds.get(name.as_str()) {
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

    #[test]
    fn a_self_bounded_constructor_variable_completes_to_its_starred_bound() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let recursive = Ty::obj_args("sample/Builder", &[Ty::ty_param("SELF", any)]);
        let comparable = Ty::obj_args(
            "kotlin/Comparable",
            &[Ty::ty_param("C", Ty::obj("kotlin/Any"))],
        );
        let plain = Ty::obj("sample/Settings");
        let formals = ["SELF".to_string(), "C".to_string(), "S".to_string()];

        assert_eq!(
            completed_constructor_result_arguments(
                crate::types::type_name("sample/Builder"),
                &formals,
                &[vec![recursive], vec![comparable], vec![plain]],
                &GSigBinds::default(),
            ),
            vec![
                Ty::out_projection(Ty::obj_args(
                    "sample/Builder",
                    &[Ty::star_projection(Ty::obj_args(
                        "sample/Builder",
                        &[Ty::star_projection(any)],
                    ))],
                )),
                comparable,
                plain,
            ]
        );
    }

    /// A recursive bound that also names another formal is not completed, even with that formal
    /// inferred: kotlinc reports `cannot infer type for type parameter 'SELF'` for `MB("v")`.
    #[test]
    fn a_self_bound_naming_another_formal_is_not_completed() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let recursive = Ty::obj_args(
            "sample/MB",
            &[Ty::ty_param("T", any), Ty::ty_param("SELF", any)],
        );
        let formals = ["T".to_string(), "SELF".to_string()];
        let inferred = GSigBinds::from_iter([("T".to_string(), Ty::obj("kotlin/String"))]);

        assert_eq!(
            completed_constructor_result_arguments(
                crate::types::type_name("sample/MB"),
                &formals,
                &[vec![any], vec![recursive]],
                &inferred,
            ),
            vec![Ty::obj("kotlin/String"), recursive]
        );
    }
}
