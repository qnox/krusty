//! Fresh call-site inference variables for a callee whose formals are lexically in scope.
//!
//! Type-parameter identities are declaration-owned: `class Tree<T>` has one `T`, shared by the
//! class body and by the constructor's generic signature. Kotlin nevertheless gives every call a
//! fresh type variable for each formal of the called declaration. Inside `Tree`, the constructor
//! call `Tree(label)` with `label: T` therefore constrains a fresh `T'` from below by the
//! enclosing, fixed `T` and solves `T' := T`. When the callee's signature is used unrenamed, that
//! constraint reads as `T` against itself, which inference correctly treats as no evidence for a
//! postponed variable, and the call's variable falls back to its default.
//!
//! The collision exists exactly when a callee formal is lexically visible at the call site: a
//! constructor of an enclosing generic class, or a generic function called from its own body.
//! [`CallSiteVariables`] renames those formals to call-owned identities for constraint solving
//! and maps the solution back onto the declaration's formals, so callers keep substituting the
//! declaration-owned signature. Formals that are not in scope keep their identity, and a
//! signature with none in scope is used unchanged.

use super::call_constraints::{GenericBoundViolation, InferredCallBindings};
use super::{GenericSig, Ty};
use std::collections::HashMap;

/// A generic signature whose lexically visible formals were renamed for one call.
pub(crate) struct CallSiteVariables {
    signature: GenericSig,
    /// Call-owned identity -> declaration-owned identity.
    declared: HashMap<&'static str, &'static str>,
}

impl CallSiteVariables {
    /// Rename every formal of `signature` for which `lexically_visible` holds. `None` when no
    /// formal is visible at the call site: the declaration's signature is then already a correct
    /// set of call variables.
    pub(crate) fn instantiate(
        signature: &GenericSig,
        lexically_visible: impl Fn(&str) -> bool,
    ) -> Option<Self> {
        let fresh = signature
            .formals
            .iter()
            .filter(|formal| lexically_visible(formal))
            .map(|formal| {
                let declared = crate::types::intern(formal);
                (declared, crate::types::call_site_type_variable(declared))
            })
            .collect::<HashMap<&str, &'static str>>();
        if fresh.is_empty() {
            return None;
        }
        let rename = |ty: Ty| crate::types::ty_rename_params(ty, &fresh);
        let renamed = GenericSig {
            formals: signature
                .formals
                .iter()
                .map(|formal| {
                    fresh
                        .get(formal.as_str())
                        .map_or_else(|| formal.clone(), |identity| (*identity).to_string())
                })
                .collect(),
            formal_bounds: signature
                .formal_bounds
                .iter()
                .map(|bounds| bounds.iter().copied().map(rename).collect())
                .collect(),
            receiver: signature.receiver.map(rename),
            params: signature.params.iter().copied().map(rename).collect(),
            ret: rename(signature.ret),
            return_policy: signature.return_policy,
        };
        let declared = fresh
            .into_iter()
            .map(|(declared, fresh)| (fresh, crate::types::intern(declared)))
            .collect();
        Some(Self {
            signature: renamed,
            declared,
        })
    }

    /// Solve one call's constraints with `infer`, against fresh variables for every formal that is
    /// lexically visible at the call site, and express the solution over the declaration's formals.
    pub(crate) fn solve(
        signature: &GenericSig,
        lexically_visible: impl Fn(&str) -> bool,
        infer: impl FnOnce(&GenericSig) -> InferredCallBindings,
    ) -> InferredCallBindings {
        match Self::instantiate(signature, lexically_visible) {
            Some(call) => call.declared_bindings(infer(call.signature())),
            None => infer(signature),
        }
    }

    /// The signature to collect and solve this call's constraints against.
    pub(crate) fn signature(&self) -> &GenericSig {
        &self.signature
    }

    /// Express a solution for [`Self::signature`] over the declaration's own formals.
    pub(crate) fn declared_bindings(&self, inferred: InferredCallBindings) -> InferredCallBindings {
        let ty = |ty: Ty| crate::types::ty_rename_params(ty, &self.declared);
        let formal = |formal: String| {
            self.declared
                .get(formal.as_str())
                .map_or(formal, |declared| (*declared).to_string())
        };
        InferredCallBindings {
            bindings: inferred
                .bindings
                .into_iter()
                .map(|(name, binding)| (formal(name), ty(binding)))
                .collect(),
            lower_inputs: inferred
                .lower_inputs
                .into_iter()
                .map(|(name, inputs)| (formal(name), inputs.into_iter().map(ty).collect()))
                .collect(),
            upper_only: inferred.upper_only.into_iter().map(formal).collect(),
            upper_bounds: inferred
                .upper_bounds
                .into_iter()
                .map(|(name, bounds)| (formal(name), bounds.into_iter().map(ty).collect()))
                .collect(),
            bound_violation: inferred
                .bound_violation
                .map(|violation| GenericBoundViolation {
                    argument: violation.argument,
                    expected: ty(violation.expected),
                    actual: ty(violation.actual),
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{infer_generic_call_constraints_from_symbols, GSigBinds};
    use super::*;
    use crate::libraries::{EmptySymbolSource, GenericReturnPolicy};

    /// `class Cell<E>(item: E)` as its constructor's generic signature.
    fn cell_constructor(formal: &str) -> GenericSig {
        let item = Ty::ty_param(formal, Ty::nullable(Ty::obj("kotlin/Any")));
        GenericSig {
            formals: vec![formal.to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: vec![item],
            ret: Ty::obj_args("demo/Cell", &[item]),
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    fn solve(signature: &GenericSig, actual: Ty, visible: bool) -> InferredCallBindings {
        CallSiteVariables::solve(
            signature,
            |_| visible,
            |signature| {
                infer_generic_call_constraints_from_symbols(
                    &EmptySymbolSource,
                    signature,
                    [(0, actual, false)],
                    None,
                )
            },
        )
    }

    #[test]
    fn an_enclosing_fixed_formal_solves_the_calls_fresh_variable() {
        let signature = cell_constructor("demo:Cell:E");
        let enclosing = signature.params[0];

        let inferred = solve(&signature, enclosing, true);

        assert_eq!(
            inferred.bindings,
            GSigBinds::from([("demo:Cell:E".to_string(), enclosing)])
        );
        assert_eq!(
            inferred.lower_inputs.get("demo:Cell:E"),
            Some(&vec![enclosing])
        );
    }

    #[test]
    fn a_formal_out_of_scope_keeps_its_declaration_identity() {
        let signature = cell_constructor("demo:Cell:E");

        assert!(CallSiteVariables::instantiate(&signature, |_| false).is_none());
        // The same identity arriving from outside the declaration is a postponed variable, not
        // evidence: nothing is solved.
        assert!(solve(&signature, signature.params[0], false)
            .bindings
            .is_empty());
        assert_eq!(
            solve(&signature, Ty::String, false).bindings,
            GSigBinds::from([("demo:Cell:E".to_string(), Ty::String)])
        );
    }

    #[test]
    fn only_visible_formals_are_renamed_and_solutions_return_to_declared_formals() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let first = Ty::ty_param("demo:Couple:A", any);
        let second = Ty::ty_param("demo:Couple:B", any);
        let signature = GenericSig {
            formals: vec!["demo:Couple:A".to_string(), "demo:Couple:B".to_string()],
            formal_bounds: vec![Vec::new(), Vec::new()],
            receiver: None,
            params: vec![first, second],
            ret: Ty::obj_args("demo/Couple", &[first, second]),
            return_policy: GenericReturnPolicy::Exact,
        };

        let call = CallSiteVariables::instantiate(&signature, |formal| formal == "demo:Couple:A")
            .expect("one visible formal");
        let renamed = call.signature();
        assert_ne!(renamed.formals[0], signature.formals[0]);
        assert_eq!(renamed.formals[1], signature.formals[1]);
        assert_eq!(renamed.params[1], second);
        assert_eq!(
            renamed.ret,
            Ty::obj_args("demo/Couple", &[renamed.params[0], second])
        );

        let inferred = call.declared_bindings(infer_generic_call_constraints_from_symbols(
            &EmptySymbolSource,
            renamed,
            [(0, first, false), (1, Ty::String, false)],
            None,
        ));
        assert_eq!(
            inferred.bindings,
            GSigBinds::from([
                ("demo:Couple:A".to_string(), first),
                ("demo:Couple:B".to_string(), Ty::String),
            ])
        );
    }
}
