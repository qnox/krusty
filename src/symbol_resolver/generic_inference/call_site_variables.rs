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
//! [`CallSiteVariables`] replaces those formals with the call's inference variables
//! ([`TyKind::Var`](crate::types::TyKind::Var)) for constraint solving and maps the solution back
//! onto the declaration's formals, so callers keep substituting the declaration-owned signature.
//! Formals that are not in scope keep their identity, and a signature with none in scope is used
//! unchanged.

use super::call_constraints::{GenericBoundViolation, InferredCallBindings};
use super::{GenericSig, Ty};
use crate::fx_hash::HashMap;
use crate::types::{TyKind, TyParamName};

/// The identity translation shared by every constraint phase of one call. Declaration formals
/// that are also visible in the caller receive call-owned identities; all other formals retain
/// their declaration identity.
#[derive(Clone, Default)]
pub(crate) struct CallSiteVariableMap {
    call_owned: HashMap<&'static str, &'static str>,
    declared: HashMap<&'static str, &'static str>,
    /// Whether the call-owned identities are inference variables. A postponed call's identities
    /// are still renamed type parameters, because the lambdas analysed before that call is solved
    /// see them in their own checked types.
    variables: bool,
}

impl CallSiteVariableMap {
    fn from_call_owned<F: crate::types::TyParamKey>(
        formals: &[F],
        variables: bool,
        mut call_owned_identity: impl FnMut(&'static str) -> Option<&'static str>,
    ) -> Self {
        let call_owned = formals
            .iter()
            .filter_map(|formal| {
                let declared = crate::types::intern(formal.borrow());
                call_owned_identity(declared).map(|call_owned| (declared, call_owned))
            })
            .collect::<HashMap<_, _>>();
        let declared = call_owned
            .iter()
            .map(|(&declared, &call_owned)| (call_owned, declared))
            .collect();
        Self {
            call_owned,
            declared,
            variables,
        }
    }

    pub(crate) fn new<F: crate::types::TyParamKey>(
        formals: &[F],
        lexically_visible: impl Fn(&str) -> bool,
    ) -> Self {
        Self::from_call_owned(formals, true, |declared| {
            lexically_visible(declared).then(|| crate::types::call_site_type_variable(declared))
        })
    }

    /// Give every still-open formal an identity owned by one postponed call expression. Repeated
    /// construction for different lambda arguments of that call returns the same identities;
    /// a nested call to the same declaration receives different ones.
    pub(crate) fn for_postponed_call<F: crate::types::TyParamKey>(
        formals: &[F],
        compilation: u64,
        file: u32,
        expression: u32,
    ) -> Self {
        Self::from_call_owned(formals, false, |declared| {
            Some(crate::types::postponed_call_type_variable(
                compilation,
                file,
                expression,
                declared,
            ))
        })
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.call_owned.is_empty()
    }

    /// `ty` with every call-owned formal replaced by this call's variable for it.
    pub(crate) fn instantiate_type(&self, ty: Ty) -> Ty {
        if !self.variables {
            return crate::types::ty_rename_params(ty, &self.call_owned);
        }
        crate::types::ty_map_symbols(ty, &|symbol, bound| match symbol.kind() {
            TyKind::TyParam(name, _) => match self.call_owned.get(name.as_str()) {
                Some(&variable) => Ty::type_variable(TyParamName::new(variable), bound),
                None => symbol.with_symbol_bound(bound),
            },
            _ => symbol.with_symbol_bound(bound),
        })
    }

    /// `ty` with this call's variables expressed as the declaration's formals again.
    pub(crate) fn declared_type(&self, ty: Ty) -> Ty {
        if !self.variables {
            return crate::types::ty_rename_params(ty, &self.declared);
        }
        crate::types::ty_map_symbols(ty, &|symbol, bound| match symbol.kind() {
            TyKind::Var(name, _) => match self.declared.get(name.as_str()) {
                Some(&formal) => Ty::ty_param(formal, bound),
                None => symbol.with_symbol_bound(bound),
            },
            _ => symbol.with_symbol_bound(bound),
        })
    }

    pub(crate) fn instantiate_formal(&self, formal: &str) -> crate::types::TyParamName {
        crate::types::TyParamName::new(self.call_owned.get(formal).copied().unwrap_or(formal))
    }

    pub(crate) fn declared_formal(&self, formal: &str) -> crate::types::TyParamName {
        crate::types::TyParamName::new(self.declared.get(formal).copied().unwrap_or(formal))
    }
}

/// A generic signature whose lexically visible formals were renamed for one call.
pub(crate) struct CallSiteVariables {
    signature: GenericSig,
    variables: CallSiteVariableMap,
}

impl CallSiteVariables {
    /// Rename every formal of `signature` for which `lexically_visible` holds. `None` when no
    /// formal is visible at the call site: the declaration's signature is then already a correct
    /// set of call variables.
    pub(crate) fn instantiate(
        signature: &GenericSig,
        lexically_visible: impl Fn(&str) -> bool,
    ) -> Option<Self> {
        let variables = CallSiteVariableMap::new(&signature.formals, lexically_visible);
        if variables.is_identity() {
            return None;
        }
        let rename = |ty: Ty| variables.instantiate_type(ty);
        let renamed = GenericSig {
            formals: signature
                .formals
                .iter()
                .map(|formal| variables.instantiate_formal(formal))
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
        Some(Self {
            signature: renamed,
            variables,
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
        let ty = |ty: Ty| self.variables.declared_type(ty);
        let formal = |formal: String| self.variables.declared_formal(&formal).to_string();
        InferredCallBindings {
            bindings: inferred
                .bindings
                .into_iter()
                .map(|(name, binding)| (self.variables.declared_formal(&name), ty(binding)))
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
            nullable_flexible_roots: inferred
                .nullable_flexible_roots
                .into_iter()
                .map(formal)
                .collect(),
            bound_violation: inferred
                .bound_violation
                .map(|violation| GenericBoundViolation {
                    expected: ty(violation.expected),
                    actual: ty(violation.actual),
                    ..violation
                }),
            bound_violations: inferred
                .bound_violations
                .into_iter()
                .map(|violation| GenericBoundViolation {
                    expected: ty(violation.expected),
                    actual: ty(violation.actual),
                    ..violation
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{infer_generic_call_constraints_from_symbols, GSigBinds};
    use super::*;
    use crate::libraries::{EmptySymbolSource, GenericReturnPolicy};
    use crate::types::TyKind;

    /// `class Cell<E>(item: E)` as its constructor's generic signature.
    fn cell_constructor(formal: &str) -> GenericSig {
        let item = Ty::ty_param(formal, Ty::nullable(Ty::obj("kotlin/Any")));
        GenericSig {
            formals: vec![crate::types::TyParamName::new(formal)],
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
            GSigBinds::from_iter([("demo:Cell:E".into(), enclosing)])
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
            GSigBinds::from_iter([("demo:Cell:E".into(), Ty::String)])
        );
    }

    #[test]
    fn only_visible_formals_are_renamed_and_solutions_return_to_declared_formals() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let first = Ty::ty_param("demo:Couple:A", any);
        let second = Ty::ty_param("demo:Couple:B", any);
        let signature = GenericSig {
            formals: vec![
                crate::types::TyParamName::new("demo:Couple:A"),
                crate::types::TyParamName::new("demo:Couple:B"),
            ],
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
        // The visible formal is the call's inference variable, not a renamed type parameter; the
        // formal out of scope stays the declaration's own parameter.
        assert!(
            matches!(renamed.params[0].kind(), TyKind::Var(name, _) if name == renamed.formals[0])
        );
        assert!(matches!(renamed.params[1].kind(), TyKind::TyParam(..)));
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
            GSigBinds::from_iter([
                ("demo:Couple:A".into(), first),
                ("demo:Couple:B".into(), Ty::String),
            ])
        );
    }

    #[test]
    fn returning_a_call_owned_solution_preserves_definitely_non_null_provenance() {
        let any = Ty::obj("kotlin/Any");
        let formal_name = "demo:Owner:R";
        let formal = Ty::ty_param(formal_name, any);
        let signature = GenericSig {
            formals: vec![crate::types::TyParamName::new(formal_name)],
            formal_bounds: vec![vec![any]],
            receiver: None,
            params: vec![Ty::nullable(formal)],
            ret: Ty::nullable(formal),
            return_policy: GenericReturnPolicy::Exact,
        };
        let caller = Ty::ty_param("caller:T", Ty::nullable(any));
        let call = CallSiteVariables::instantiate(&signature, |_| true)
            .expect("the visible formal must get a call-owned identity");

        let inferred = infer_generic_call_constraints_from_symbols(
            &EmptySymbolSource,
            call.signature(),
            [(0, caller, false)],
            None,
        );
        let declared = call.declared_bindings(inferred);
        let solved = declared.bindings[formal_name];

        assert!(matches!(solved.kind(), TyKind::DefinitelyNotNull(_)));
        assert_eq!(Ty::nullable(solved), Ty::nullable(caller));
        assert_eq!(
            Ty::nullable(solved).non_null().ty_param_bound(),
            Some(Ty::nullable(any))
        );
    }

    #[test]
    fn a_variable_returns_to_its_formal_with_its_bound_chain() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let upper = Ty::ty_param("demo:Pair:A", any);
        let lower = Ty::ty_param("demo:Pair:B", upper);
        let declared = Ty::obj_args("demo/Pair", &[upper, Ty::nullable(lower)]);
        let variables = CallSiteVariableMap::new(
            &[
                crate::types::TyParamName::new("demo:Pair:A"),
                crate::types::TyParamName::new("demo:Pair:B"),
            ],
            |_| true,
        );

        let instantiated = variables.instantiate_type(declared);
        let TyKind::Obj(_, arguments) = instantiated.kind() else {
            panic!("the classifier application keeps its shape");
        };
        let TyKind::Var(_, bound) = arguments[1].non_null().kind() else {
            panic!("a visible formal becomes the call's variable");
        };
        // The bound of one variable names the call's variable for the other formal, not the
        // enclosing declaration's fixed parameter.
        assert!(matches!(bound.kind(), TyKind::Var(..)));
        assert_eq!(arguments[0], bound);
        assert_eq!(variables.declared_type(instantiated), declared);
    }
}
