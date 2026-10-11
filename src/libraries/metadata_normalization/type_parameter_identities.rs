//! Declaration-owned identities of type parameters decoded from dependency metadata.

use std::collections::HashMap;

use crate::metadata::semantic::{KotlinTypeParameter, KotlinTypeParameterId};

/// Opaque semantic identities keyed by metadata's declaration-scoped parameter IDs.
///
/// A provider constructs this only after it has established the declaration's exact external
/// identity, and retains it with that signed declaration. Written names remain diagnostic data;
/// two declarations that both spell a parameter `T` therefore never share an inference variable.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeParameterIdentities {
    by_id: HashMap<KotlinTypeParameterId, &'static str>,
    by_source: HashMap<String, &'static str>,
    formals: Vec<String>,
}

impl TypeParameterIdentities {
    /// Assign identities to one declaration's own formals while retaining the identities its
    /// enclosing classifiers put in scope. A repeated metadata ID or source spelling is shadowed
    /// by the inner declaration, exactly as the metadata scope is.
    pub(crate) fn from_provider_with_enclosing(
        formals: &[KotlinTypeParameter],
        enclosing: Option<&Self>,
        mut identity: impl FnMut(usize, &KotlinTypeParameter) -> &'static str,
    ) -> Self {
        let mut by_id = enclosing
            .map(|identities| identities.by_id.clone())
            .unwrap_or_default();
        let mut by_source = enclosing
            .map(|identities| identities.by_source.clone())
            .unwrap_or_default();
        by_id.reserve(formals.len());
        by_source.reserve(formals.len());
        let mut identities = Vec::with_capacity(formals.len());
        for (ordinal, parameter) in formals.iter().enumerate() {
            let identity = identity(ordinal, parameter);
            by_id.insert(parameter.id, identity);
            by_source.insert(parameter.name.clone(), identity);
            identities.push(identity.to_owned());
        }
        Self {
            by_id,
            by_source,
            formals: identities,
        }
    }

    pub(crate) fn get(&self, id: KotlinTypeParameterId) -> Option<&'static str> {
        self.by_id.get(&id).copied()
    }

    pub(crate) fn by_id(&self) -> &HashMap<KotlinTypeParameterId, &'static str> {
        &self.by_id
    }

    pub(crate) fn formals(&self) -> &[String] {
        &self.formals
    }

    pub(crate) fn normalize_contract(
        &self,
        contract: &std::sync::Arc<crate::contracts::Contract>,
    ) -> std::sync::Arc<crate::contracts::Contract> {
        use crate::contracts::{Condition, ConditionType, Contract, Effect};

        let identities = self
            .by_source
            .iter()
            .map(|(source, identity)| (source.as_str(), *identity))
            .collect();
        fn condition(original: &Condition, identities: &HashMap<&str, &'static str>) -> Condition {
            match original {
                Condition::IsType { param, ty, negated } => Condition::IsType {
                    param: *param,
                    ty: match ty {
                        ConditionType::Metadata(ty) => {
                            ConditionType::Metadata(crate::types::ty_rename_params(*ty, identities))
                        }
                        ConditionType::Source(source) => ConditionType::Source(source.clone()),
                    },
                    negated: *negated,
                },
                Condition::And(left, right) => Condition::And(
                    Box::new(condition(left, identities)),
                    Box::new(condition(right, identities)),
                ),
                Condition::Or(left, right) => Condition::Or(
                    Box::new(condition(left, identities)),
                    Box::new(condition(right, identities)),
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
                        conclusion: condition(conclusion, &identities),
                    },
                    Effect::HoldsIn {
                        condition: holds,
                        lambda,
                    } => Effect::HoldsIn {
                        condition: condition(holds, &identities),
                        lambda: *lambda,
                    },
                    other => other.clone(),
                })
                .collect(),
        })
    }
}
