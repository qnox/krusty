//! The generic signature kotlinc writes for a member that overrides a mapped collection declaration
//! with a signature policy.
//!
//! kotlinc's JVM signature mapper writes the realization of a few builtin collection members other
//! than as declared, and every override inherits that: an override of `Collection.containsAll` or
//! `MutableCollection.removeAll`/`retainAll` (`ERASED_COLLECTION_PARAMETER_SIGNATURES`) takes its
//! collection parameter star-projected, `containsAll(Collection<?>)`, and an override of
//! `MutableCollection.addAll`, `MutableList.addAll` or `MutableMap.putAll`
//! (`METHODS_WITH_DECLARATION_SITE_WILDCARDS`) writes every declaration-site variance of its
//! parameters, `addAll(Collection<? extends String>)`. An override of a type-safe declaration
//! (`contains`, `indexOf`, `Map.get`, ...) that keeps the declaration's erased descriptor writes
//! the checked parameters as `Object`. The policy belongs to the overridden
//! declaration, keyed by its owner, name and erased descriptor and found through the class's exact
//! override edges and the builtin supertypes of that owner.

use crate::fx_hash::HashMap;
use crate::ir::{FunId, IrFile};
use crate::jvm::classpath::Classpath;
use crate::jvm::mapped_builtin_declarations::{
    collection_bridge_policy_for_declaration, collection_signature_policy_for_declaration,
    CollectionSignaturePolicy, MappedBuiltinMemberKind,
};
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::names::method_descriptor;
use crate::jvm::read_only_collection_stubs::overridden_declaration_policy;

/// How kotlinc signs an override of a collection declaration with a signature policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OverrideSignature {
    /// The declaration's [`CollectionSignaturePolicy`].
    Policy(CollectionSignaturePolicy),
    /// A type-safe declaration (`contains`, `Map.get`) whose override keeps the declaration's
    /// erased descriptor: the checked parameters are written as the erased `Object`, so
    /// `contains(T)` of a generic class carries no `Signature` and `get(K)` is `(Ljava/lang/Object;)TV;`.
    /// An override with another descriptor keeps its own types; its special bridge does the check.
    ErasedCheckedParameters(&'static [u16]),
}

/// The signature of each function of this file that overrides a declaration with a policy.
#[derive(Default)]
pub(crate) struct CollectionOverrideSignatures {
    by_function: HashMap<FunId, OverrideSignature>,
}

impl CollectionOverrideSignatures {
    pub(super) fn policy(&self, function: FunId) -> Option<OverrideSignature> {
        self.by_function.get(&function).copied()
    }
}

pub(super) fn select(ir: &IrFile, classpath: &Classpath) -> CollectionOverrideSignatures {
    let mut result = CollectionOverrideSignatures::default();
    for (class_index, class) in ir.classes.iter().enumerate() {
        if !crate::jvm::override_results::realizes_overrides(ir, class_index) {
            continue;
        }
        let owner = class.fq_name;
        let Some(edges) = ir.function_overrides.get(&owner) else {
            continue;
        };
        for edge in edges {
            if edge.implementation_owner != owner || edge.suspend {
                continue;
            }
            let Some(function) = crate::jvm::bridges::implementation_function(ir, edge) else {
                continue;
            };
            if !class.methods.contains(&function) || result.by_function.contains_key(&function) {
                continue;
            }
            let declared_parameters = jvm_tys(&edge.declared_parameters);
            let descriptor =
                method_descriptor(&declared_parameters, jvm_declared_ty(&edge.declared_result));
            let policy = overridden_declaration_policy(classpath, edge.overridden_owner, |owner| {
                collection_signature_policy_for_declaration(
                    owner,
                    &edge.name,
                    &descriptor,
                    MappedBuiltinMemberKind::Function,
                )
            })
            .map(OverrideSignature::Policy)
            .or_else(|| {
                let type_safe =
                    overridden_declaration_policy(classpath, edge.overridden_owner, |owner| {
                        collection_bridge_policy_for_declaration(
                            owner,
                            &edge.name,
                            &descriptor,
                            MappedBuiltinMemberKind::Function,
                        )
                    })?;
                (jvm_tys(&ir.functions[function as usize].params) == declared_parameters).then_some(
                    OverrideSignature::ErasedCheckedParameters(type_safe.checked_parameters),
                )
            });
            if let Some(policy) = policy {
                result.by_function.insert(function, policy);
            }
        }
    }
    result
}
