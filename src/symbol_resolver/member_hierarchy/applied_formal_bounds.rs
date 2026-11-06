//! The applied member's own type-parameter bounds after its receiver's substitution.

use crate::fx_hash::HashMap;
use crate::libraries::GenericSig;
use crate::types::{ty_mentions_param, ty_with_transitive_param_bounds, Ty, TyParamName};

/// Refresh the applied member's own bound graph after the receiver substitution, returning every
/// formal whose bound it changed with its refreshed primary bound. kotlinc's fake override
/// declares fresh type parameters with the substituted bounds, so every occurrence of such a
/// formal in the applied member carries the new bound (`fun <T : E> f(x: T)` seen through
/// `I<String>` takes a `T : String`). A formal bounded by a refreshed one is itself refreshed:
/// `fun <T : E, U : T>` seen through `I<String>` takes a `U : T : String`, although `U`'s
/// declared bound mentions no supertype argument. The graph is hydrated with the cycle-safe
/// transitive bound operation, so an F-bound (`T : Comparable<T>`) terminates.
pub(super) fn refreshed_formal_bounds(
    signature: &mut GenericSig,
    declared_bounds: &[Ty],
) -> HashMap<TyParamName, Ty> {
    let mut affected = signature
        .formals
        .iter()
        .zip(declared_bounds)
        .enumerate()
        .filter(|(ordinal, (_, declared))| signature.primary_formal_bound(*ordinal) != **declared)
        .map(|(_, (formal, _))| *formal)
        .collect::<Vec<_>>();
    if affected.is_empty() {
        return HashMap::default();
    }
    loop {
        let dependents = signature
            .formals
            .iter()
            .zip(&signature.formal_bounds)
            .filter(|(formal, bounds)| {
                !affected.contains(formal)
                    && bounds
                        .iter()
                        .any(|bound| ty_mentions_param(*bound, &affected))
            })
            .map(|(formal, _)| *formal)
            .collect::<Vec<_>>();
        if dependents.is_empty() {
            break;
        }
        affected.extend(dependents);
    }
    let applied = signature
        .formals
        .iter()
        .enumerate()
        .filter(|(_, formal)| affected.contains(formal))
        .map(|(ordinal, formal)| (*formal, signature.primary_formal_bound(ordinal)))
        .collect::<HashMap<_, _>>();
    for (formal, bounds) in signature.formals.iter().zip(&mut signature.formal_bounds) {
        if affected.contains(formal) {
            for bound in bounds {
                *bound = ty_with_transitive_param_bounds(*bound, &applied);
            }
        }
    }
    signature
        .formals
        .iter()
        .enumerate()
        .filter(|(_, formal)| affected.contains(formal))
        .map(|(ordinal, formal)| (*formal, signature.primary_formal_bound(ordinal)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(formals: &[&str], formal_bounds: Vec<Vec<Ty>>) -> GenericSig {
        GenericSig {
            formals: TyParamName::all(formals),
            formal_bounds,
            receiver: None,
            params: Vec::new(),
            ret: Ty::obj("kotlin/Unit"),
            return_policy: Default::default(),
        }
    }

    /// `fun <T : E, U : T>` seen through `Source<String>`: the substitution rewrote `T`'s bound
    /// only, and `U`'s bound graph is refreshed through it.
    #[test]
    fn a_formal_bounded_by_a_refreshed_formal_is_refreshed_through_it() {
        let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
        let class_formal = Ty::ty_param("E", implicit);
        let declared_t = Ty::ty_param("T", class_formal);
        let string = Ty::obj("kotlin/String");
        let mut applied = signature(&["T", "U"], vec![vec![string], vec![declared_t]]);

        let refreshed = refreshed_formal_bounds(&mut applied, &[class_formal, declared_t]);

        let refreshed_t = Ty::ty_param("T", string);
        assert_eq!(
            refreshed,
            HashMap::from_iter([
                (TyParamName::new("T"), string),
                (TyParamName::new("U"), refreshed_t),
            ])
        );
        assert_eq!(applied.formal_bounds, vec![vec![string], vec![refreshed_t]]);
        assert_eq!(
            ty_with_transitive_param_bounds(Ty::ty_param("U", declared_t), &refreshed),
            Ty::ty_param("U", refreshed_t)
        );
    }

    /// A formal the substitution leaves alone, and that no refreshed formal bounds, keeps its
    /// occurrences; a self-referencing bound refreshed through another formal terminates.
    #[test]
    fn an_unaffected_formal_is_kept_and_a_self_referencing_bound_terminates() {
        let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
        let mut untouched = signature(&["V"], vec![vec![]]);
        assert!(refreshed_formal_bounds(&mut untouched, &[implicit]).is_empty());

        let class_formal = Ty::ty_param("E", implicit);
        let declared_t = Ty::ty_param("T", class_formal);
        let string = Ty::obj("kotlin/String");
        let pair = |first: Ty, second: Ty| Ty::obj_args("kotlin/Pair", &[first, second]);
        let declared_s = pair(declared_t, Ty::ty_param("S", implicit));
        let mut applied = signature(&["T", "S"], vec![vec![string], vec![declared_s]]);

        let refreshed = refreshed_formal_bounds(&mut applied, &[class_formal, declared_s]);

        let refreshed_t = Ty::ty_param("T", string);
        let refreshed_s = pair(
            refreshed_t,
            Ty::ty_param("S", pair(refreshed_t, Ty::ty_param("S", implicit))),
        );
        assert_eq!(
            refreshed,
            HashMap::from_iter([
                (TyParamName::new("T"), string),
                (TyParamName::new("S"), refreshed_s)
            ])
        );
    }
}
