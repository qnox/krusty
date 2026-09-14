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
}
