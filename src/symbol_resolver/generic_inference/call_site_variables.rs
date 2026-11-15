//! The inference variables one call solves.
//!
//! Type-parameter identities are declaration-owned: `class Tree<T>` has one `T`, shared by the
//! class body and by the constructor's generic signature. Kotlin gives every call a fresh type
//! variable for each formal of the called declaration. Inside `Tree`, the constructor call
//! `Tree(label)` with `label: T` constrains a fresh `T'` from below by the enclosing, fixed `T` and
//! solves `T' := T`; read against the unrenamed signature, that constraint would be `T` against
//! itself. Two calls to one declaration in one expression (`f(f(x))`) likewise own different
//! variables.
//!
//! [`CallSiteVariables`] replaces every formal of the callee with the call's inference variable
//! ([`TyKind::Var`](crate::types::TyKind::Var)), keyed by the call expression, for constraint
//! solving, and maps the solution back onto the declaration's formals, so callers keep
//! substituting the declaration-owned signature.

use super::call_constraints::{GenericBoundViolation, InferredCallBindings};
use super::{GenericSig, Ty};
use crate::fx_hash::HashMap;
use crate::types::{TyKind, TyParamName};

/// The identity translation shared by every constraint phase of one call: each declaration formal
/// and the variable the call owns for it.
#[derive(Clone, Default)]
pub(crate) struct CallSiteVariableMap {
    call_owned: HashMap<&'static str, &'static str>,
    declared: HashMap<&'static str, &'static str>,
}

impl CallSiteVariableMap {
    fn from_call_owned<F: crate::types::TyParamKey>(
        formals: &[F],
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
        }
    }

    /// Give every formal the inference variable call `site` owns for it.
    pub(crate) fn for_call<F: crate::types::TyParamKey>(
        formals: &[F],
        site: crate::types::CallSiteIdentity,
    ) -> Self {
        Self::from_call_owned(formals, |declared| {
            Some(crate::types::call_site_type_variable(site, declared))
        })
    }

    /// Give every still-open formal an identity owned by one postponed call expression. Repeated
    /// construction for different lambda arguments of that call returns the same identities, and
    /// records `argument` as one of the function arguments analysed under them; a nested call to
    /// the same declaration receives different ones.
    pub(crate) fn for_postponed_call<F: crate::types::TyParamKey>(
        formals: &[F],
        site: crate::types::CallSiteIdentity,
        argument: crate::diag::Span,
    ) -> Self {
        Self::from_call_owned(formals, |declared| {
            Some(crate::types::postponed_call_type_variable(
                site, argument, declared,
            ))
        })
    }

    /// `ty` with every call-owned formal replaced by this call's variable for it. A formal of a
    /// signature that already names inference variables (a candidate instantiated for a
    /// lexically fixed formal) is such a variable, and is renamed like a declared one.
    pub(crate) fn instantiate_type(&self, ty: Ty) -> Ty {
        crate::types::ty_map_symbols(ty, &|symbol, bound| match symbol.kind() {
            TyKind::TyParam(name, _) | TyKind::Var(name, _) => {
                match self.call_owned.get(name.as_str()) {
                    Some(&variable) => Ty::type_variable(TyParamName::new(variable), bound),
                    None => symbol.with_symbol_bound(bound),
                }
            }
            _ => symbol.with_symbol_bound(bound),
        })
    }

    /// `ty` with this call's variables expressed as the declaration's formals again.
    pub(crate) fn declared_type(&self, ty: Ty) -> Ty {
        crate::types::ty_map_symbols(ty, &|symbol, bound| match symbol.kind() {
            TyKind::Var(name, _) => match self.declared.get(name.as_str()) {
                Some(&formal) => Ty::ty_param(formal, bound),
                None => symbol.with_symbol_bound(bound),
            },
            _ => symbol.with_symbol_bound(bound),
        })
    }

    /// `bindings` of this call's variables as bindings of the declaration's formals.
    pub(crate) fn declared_bindings(&self, bindings: super::GSigBinds) -> super::GSigBinds {
        bindings
            .into_iter()
            .map(|(name, binding)| (self.declared_formal(&name), self.declared_type(binding)))
            .collect()
    }

    pub(crate) fn instantiate_formal(&self, formal: &str) -> crate::types::TyParamName {
        crate::types::TyParamName::new(self.call_owned.get(formal).copied().unwrap_or(formal))
    }

    pub(crate) fn declared_formal(&self, formal: &str) -> crate::types::TyParamName {
        crate::types::TyParamName::new(self.declared.get(formal).copied().unwrap_or(formal))
    }
}

/// A generic signature whose formals were replaced by one call's inference variables.
pub(crate) struct CallSiteVariables {
    signature: GenericSig,
    variables: CallSiteVariableMap,
}

impl CallSiteVariables {
    /// `signature` with every formal replaced by the variable call `site` owns for it.
    pub(crate) fn instantiate(
        signature: &GenericSig,
        site: crate::types::CallSiteIdentity,
    ) -> Self {
        let variables = CallSiteVariableMap::for_call(&signature.formals, site);
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
        Self {
            signature: renamed,
            variables,
        }
    }

    /// Solve one call's constraints with `infer` against the call's own variables, and express the
    /// solution over the declaration's formals.
    pub(crate) fn solve(
        signature: &GenericSig,
        site: crate::types::CallSiteIdentity,
        infer: impl FnOnce(&GenericSig) -> InferredCallBindings,
    ) -> InferredCallBindings {
        let call = Self::instantiate(signature, site);
        call.declared_bindings(infer(call.signature()))
    }

    /// The identity translation between this call's variables and the declaration's formals.
    pub(crate) fn variables(&self) -> &CallSiteVariableMap {
        &self.variables
    }

    /// The signature to collect and solve this call's constraints against.
    pub(crate) fn signature(&self) -> &GenericSig {
        &self.signature
    }

    /// This call's variables for the declaration formals `formals` names, such as the formals an
    /// `@OnlyInputTypes` policy restricts.
    pub(crate) fn instantiate_formals<F: crate::types::TyParamKey>(
        &self,
        formals: &[F],
    ) -> Vec<TyParamName> {
        formals
            .iter()
            .map(|formal| self.variables.instantiate_formal(formal.borrow()))
            .collect()
    }

    /// Express a solution for [`Self::signature`] over the declaration's own formals.
    pub(crate) fn declared_bindings(&self, inferred: InferredCallBindings) -> InferredCallBindings {
        let ty = |ty: Ty| self.variables.declared_type(ty);
        let formal = |formal: String| self.variables.declared_formal(&formal).to_string();
        InferredCallBindings {
            bindings: self.variables.declared_bindings(inferred.bindings),
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
    use crate::types::{CallSiteIdentity, TyKind};

    fn site(call: u32) -> CallSiteIdentity {
        CallSiteIdentity {
            compilation: 7,
            file: 0,
            call,
        }
    }

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

    fn solve(signature: &GenericSig, actual: Ty) -> InferredCallBindings {
        CallSiteVariables::solve(signature, site(1), |signature| {
            infer_generic_call_constraints_from_symbols(
                &EmptySymbolSource,
                signature,
                [(0, actual, false)],
                None,
            )
        })
    }

    #[test]
    fn an_enclosing_fixed_formal_solves_the_calls_fresh_variable() {
        let signature = cell_constructor("demo:Cell:E");
        let enclosing = signature.params[0];

        let inferred = solve(&signature, enclosing);

        assert_eq!(
            inferred.bindings,
            GSigBinds::from_iter([("demo:Cell:E".into(), enclosing)])
        );
        assert_eq!(
            inferred.lower_inputs.get("demo:Cell:E"),
            Some(&vec![enclosing])
        );
        assert_eq!(
            solve(&signature, Ty::String).bindings,
            GSigBinds::from_iter([("demo:Cell:E".into(), Ty::String)])
        );
    }

    #[test]
    fn every_formal_becomes_a_variable_and_solutions_return_to_declared_formals() {
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

        let call = CallSiteVariables::instantiate(&signature, site(1));
        let renamed = call.signature();
        for (ordinal, formal) in renamed.formals.iter().enumerate() {
            assert_ne!(*formal, signature.formals[ordinal]);
            assert!(
                matches!(renamed.params[ordinal].kind(), TyKind::Var(name, _) if name == *formal)
            );
        }
        assert_eq!(
            renamed.ret,
            Ty::obj_args("demo/Couple", &[renamed.params[0], renamed.params[1]])
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
    fn two_calls_to_one_declaration_own_different_variables() {
        let signature = cell_constructor("demo:Cell:E");

        let inner = CallSiteVariables::instantiate(&signature, site(1));
        let outer = CallSiteVariables::instantiate(&signature, site(2));
        let again = CallSiteVariables::instantiate(&signature, site(1));

        assert_ne!(inner.signature().formals, outer.signature().formals);
        assert_eq!(inner.signature().formals, again.signature().formals);
        assert_eq!(
            crate::types::type_parameter_source_name(inner.signature().formals[0].as_str()),
            crate::types::type_parameter_source_name("demo:Cell:E")
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
        let call = CallSiteVariables::instantiate(&signature, site(1));

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

    /// `fun <@OnlyInputTypes T> Iterable<T>.contains(element: T)` keeps its policy at a call: the
    /// policy names the declaration's `T`, and the call solves its own variable for it, so
    /// `listOf(1).contains("a")` has no input type that admits both inputs.
    #[test]
    fn an_only_input_types_policy_restricts_the_calls_own_variable() {
        let formal = "kotlin.collections:contains:T";
        let element = Ty::ty_param(formal, Ty::nullable(Ty::obj("kotlin/Any")));
        let signature = GenericSig {
            formals: vec![crate::types::TyParamName::new(formal)],
            formal_bounds: vec![Vec::new()],
            receiver: Some(Ty::obj_args("kotlin/collections/Iterable", &[element])),
            params: vec![element],
            ret: Ty::Boolean,
            return_policy: GenericReturnPolicy::Exact,
        };
        let call = CallSiteVariables::instantiate(&signature, site(1));
        let policy = call.instantiate_formals(&[formal.to_owned()]);
        let receiver = Ty::obj_args("kotlin/collections/Iterable", &[Ty::Int]);
        let admits = |argument: Ty| {
            let actuals = [(
                0,
                crate::symbol_resolver::CallArgKind::Typed(argument),
                false,
            )];
            let mut bindings = GSigBinds::from_iter([(
                call.signature().formals[0],
                Ty::nullable(Ty::obj("kotlin/Any")),
            )]);
            super::super::apply_only_input_type_bindings(
                super::super::OnlyInputCall {
                    source: &EmptySymbolSource,
                    signature: call.signature(),
                    receiver: Some(receiver),
                    arguments: &actuals,
                    vararg_index: None,
                    expected_result: None,
                },
                &policy,
                0usize,
                &mut bindings,
            )
            .then(|| bindings[call.signature().formals[0].as_str()])
        };

        assert_eq!(policy, call.signature().formals);
        assert_eq!(admits(Ty::Int), Some(Ty::Int));
        assert_eq!(admits(Ty::String), None);
    }

    #[test]
    fn a_variable_returns_to_its_formal_with_its_bound_chain() {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let upper = Ty::ty_param("demo:Pair:A", any);
        let lower = Ty::ty_param("demo:Pair:B", upper);
        let declared = Ty::obj_args("demo/Pair", &[upper, Ty::nullable(lower)]);
        let variables = CallSiteVariableMap::for_call(
            &[
                crate::types::TyParamName::new("demo:Pair:A"),
                crate::types::TyParamName::new("demo:Pair:B"),
            ],
            site(1),
        );

        let instantiated = variables.instantiate_type(declared);
        let TyKind::Obj(_, arguments) = instantiated.kind() else {
            panic!("the classifier application keeps its shape");
        };
        let TyKind::Var(_, bound) = arguments[1].non_null().kind() else {
            panic!("every formal becomes the call's variable");
        };
        // The bound of one variable names the call's variable for the other formal, not the
        // enclosing declaration's fixed parameter.
        assert!(matches!(bound.kind(), TyKind::Var(..)));
        assert_eq!(arguments[0], bound);
        assert_eq!(variables.declared_type(instantiated), declared);
    }
}
