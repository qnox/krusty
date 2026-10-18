use crate::types::Ty;

use super::SemanticPlatform;

pub(crate) fn normalize_contract_type_parameters(
    contract: &std::sync::Arc<crate::contracts::Contract>,
    rename: &crate::fx_hash::HashMap<&str, &'static str>,
) -> std::sync::Arc<crate::contracts::Contract> {
    use crate::contracts::{Condition, ConditionType, Contract, Effect};

    fn condition(
        original: &Condition,
        rename: &crate::fx_hash::HashMap<&str, &'static str>,
    ) -> Condition {
        match original {
            Condition::IsType { param, ty, negated } => Condition::IsType {
                param: *param,
                ty: match ty {
                    ConditionType::Metadata(ty) => {
                        ConditionType::Metadata(crate::types::ty_rename_params(*ty, rename))
                    }
                    ConditionType::Source(source) => ConditionType::Source(source.clone()),
                },
                negated: *negated,
            },
            Condition::And(left, right) => Condition::And(
                Box::new(condition(left, rename)),
                Box::new(condition(right, rename)),
            ),
            Condition::Or(left, right) => Condition::Or(
                Box::new(condition(left, rename)),
                Box::new(condition(right, rename)),
            ),
            other => other.clone(),
        }
    }
    std::sync::Arc::new(Contract {
        effects: contract
            .effects
            .iter()
            .map(|effect| match effect {
                Effect::ConditionalReturns {
                    returns,
                    conclusion,
                } => Effect::ConditionalReturns {
                    returns: *returns,
                    conclusion: condition(conclusion, rename),
                },
                Effect::HoldsIn {
                    condition: holds,
                    lambda,
                } => Effect::HoldsIn {
                    condition: condition(holds, rename),
                    lambda: *lambda,
                },
                other => other.clone(),
            })
            .collect(),
    })
}

/// A parsed generic signature in Kotlin's logical shape: formal type-parameter names, an optional
/// receiver, the value parameters, and the return. Providers normalize their source format into
/// this one declaration-owned contract before resolution consumes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericSig {
    pub formals: Vec<String>,
    /// Declared upper bounds, parallel to [`Self::formals`]. An empty slot denotes Kotlin's
    /// implicit nullable `Any?` bound.
    pub formal_bounds: Vec<Vec<Ty>>,
    pub receiver: Option<Ty>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub return_policy: GenericReturnPolicy,
}

/// Post-substitution policy for a generic callable's return. Keeping this on [`GenericSig`] gives
/// member, static, and top-level resolution one authoritative fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GenericReturnPolicy {
    #[default]
    Exact,
    /// An unannotated reference contract remains null-capable when its outer method type parameter
    /// binds to a primitive.
    FlexibleReference,
}

impl GenericSig {
    fn declaration_formal_renames_from(
        &self,
        prior: &Self,
    ) -> crate::fx_hash::HashMap<&'static str, &'static str> {
        prior
            .formals
            .iter()
            .zip(&self.formals)
            .flat_map(|(prior, identity)| {
                let identity = crate::types::intern(identity);
                [
                    (crate::types::intern(prior), identity),
                    (crate::types::type_parameter_source_name(prior), identity),
                ]
            })
            .collect()
    }

    pub(crate) fn normalize_declaration_type_from(&self, prior: &Self, ty: Ty) -> Ty {
        crate::types::ty_rename_params(ty, &self.declaration_formal_renames_from(prior))
    }

    pub(crate) fn normalize_call_sig_from(
        &self,
        prior: &Self,
        call: &mut crate::libraries::CallSig,
    ) {
        let rename = self.declaration_formal_renames_from(prior);
        Self::normalize_call_sig_with(call, &rename);
    }

    fn normalize_call_sig_with(
        call: &mut crate::libraries::CallSig,
        rename: &crate::fx_hash::HashMap<&str, &'static str>,
    ) {
        for formal in &mut call.only_input_type_formals {
            if let Some(identity) = rename.get(formal.as_str()) {
                *formal = (*identity).to_owned();
            }
        }
        for parameters in &mut call.lambda_param_types {
            for parameter in parameters {
                *parameter = crate::types::ty_rename_params(*parameter, &rename);
            }
        }
        for receiver in call.lambda_receivers.iter_mut().flatten() {
            *receiver = crate::types::ty_rename_params(*receiver, &rename);
        }
    }

    pub(crate) fn normalize_contract_from(
        &self,
        prior: &Self,
        contract: &std::sync::Arc<crate::contracts::Contract>,
    ) -> std::sync::Arc<crate::contracts::Contract> {
        let rename = self.declaration_formal_renames_from(prior);
        normalize_contract_type_parameters(contract, &rename)
    }

    /// Replace source-spelled formals with identities assigned by the declaration provider.
    ///
    /// Parsed metadata and JVM signatures use their local names while decoding recursive bounds.
    /// A provider calls this only after it has joined that shape to one stable declaration. Every
    /// occurrence is renamed in one transaction so consumers never observe an identity-bearing
    /// formal beside a source-spelled parameter or result.
    pub(crate) fn assign_declaration_formal_identities(
        &mut self,
        mut identity: impl FnMut(usize, &str) -> &'static str,
    ) {
        let sources = self.formals.clone();
        let identities = sources
            .iter()
            .enumerate()
            .map(|(ordinal, source)| identity(ordinal, source))
            .collect::<Vec<_>>();
        let rename = sources
            .iter()
            .zip(&identities)
            .map(|(source, identity)| (source.as_str(), *identity))
            .collect::<crate::fx_hash::HashMap<_, _>>();
        for bounds in &mut self.formal_bounds {
            for bound in bounds {
                *bound = crate::types::ty_rename_params(*bound, &rename);
            }
        }
        self.receiver = self
            .receiver
            .map(|receiver| crate::types::ty_rename_params(receiver, &rename));
        for parameter in &mut self.params {
            *parameter = crate::types::ty_rename_params(*parameter, &rename);
        }
        self.ret = crate::types::ty_rename_params(self.ret, &rename);
        self.formals = identities.into_iter().map(str::to_owned).collect();
    }

    /// The first declared upper bound, or Kotlin's implicit `Any?` when none was written.
    pub fn primary_formal_bound(&self, index: usize) -> Ty {
        self.formal_bounds
            .get(index)
            .and_then(|bounds| bounds.first())
            .copied()
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
    }

    /// Rebuild the declaration's source parameters in the order used by a static realization:
    /// `(contexts..., extension receiver, values...)`.
    pub fn parameters_with_receiver(&self, context_count: usize) -> Box<[Ty]> {
        let mut parameters = self.params.clone();
        if let Some(receiver) = self.receiver {
            parameters.insert(context_count.min(parameters.len()), receiver);
        }
        parameters.into_boxed_slice()
    }

    pub fn apply_return_policy(&self, platform: &dyn SemanticPlatform, specialized: Ty) -> Ty {
        match self.return_policy {
            GenericReturnPolicy::Exact => specialized,
            GenericReturnPolicy::FlexibleReference => specialized
                .boxed_ref()
                .map(|boxed| platform.library_value_form(boxed))
                .unwrap_or(specialized),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(bounds: Vec<Vec<Ty>>) -> GenericSig {
        GenericSig {
            formals: vec!["T".to_owned()],
            formal_bounds: bounds,
            receiver: None,
            params: Vec::new(),
            ret: Ty::Unit,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn an_absent_bound_is_nullable_any_but_a_declared_bound_is_preserved() {
        assert_eq!(
            signature(vec![Vec::new()]).primary_formal_bound(0),
            Ty::nullable(Ty::obj("kotlin/Any"))
        );
        assert_eq!(
            signature(vec![vec![Ty::obj("sample/Bound")]]).primary_formal_bound(0),
            Ty::obj("sample/Bound")
        );
    }

    #[test]
    fn dependency_formals_and_parallel_shapes_move_to_one_declaration_identity() {
        let any = Ty::obj("kotlin/Any");
        let mut signature = GenericSig {
            formals: vec!["R".to_owned()],
            formal_bounds: vec![vec![Ty::ty_param("R", any)]],
            receiver: None,
            params: vec![Ty::fun(Vec::new(), Ty::ty_param("R", any))],
            ret: Ty::ty_param("R", any),
            return_policy: GenericReturnPolicy::Exact,
        };
        let first = crate::types::metadata_type_parameter("R");
        signature.assign_declaration_formal_identities(|_, _| first);
        assert_eq!(signature.formals, vec![first.to_owned()]);
        assert!(crate::types::ty_mentions_param(
            signature.ret,
            &[first.to_owned()]
        ));

        let prior = signature.clone();
        let second = crate::types::metadata_type_parameter("R");
        signature.assign_declaration_formal_identities(|_, _| second);
        let parallel = signature
            .normalize_declaration_type_from(&prior, Ty::ty_param(first, Ty::obj("kotlin/Any")));
        assert!(crate::types::ty_mentions_param(
            parallel,
            &[second.to_owned()]
        ));
        assert!(!crate::types::ty_mentions_param(
            parallel,
            &[first.to_owned()]
        ));
        assert_eq!(crate::types::type_parameter_source_name(second), "R");
    }
}
