//! Exact serialized member references normalized before dependency template lowering.

use super::{KlibCalleeOrigin, KlibLibraries};
use crate::fx_hash::{HashMap, HashSet};
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::id_signature::{package_function_signature, MetadataClass, MetadataContainer};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// The dispatch classifier named by a reference and its applied declaring superclass/interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibReferenceDispatch {
    pub receiver: Ty,
    pub declaring_receiver: Ty,
}

/// One reference's canonical declaration, without inventing a declaration on its dispatch class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibReferenceBinding {
    pub declaration: KlibDependencyCallable,
    pub dispatch: Option<KlibReferenceDispatch>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KlibReferenceOrigin {
    Exact(KlibReferenceBinding),
    Missing,
    Ambiguous,
}

/// Immutable identity bindings shared by compilations over the same archive generation.
#[derive(Default)]
pub struct KlibReferenceBindings {
    references: HashMap<(String, KlibDeclarationSignature), KlibReferenceOrigin>,
}

impl KlibReferenceBindings {
    pub(super) fn freeze(libraries: &KlibLibraries) -> Self {
        Self {
            references: libraries
                .inline_bodies
                .callable_references()
                .into_iter()
                .map(|(module, reference)| {
                    let binding = normalize_reference(libraries, &module, &reference);
                    ((module, reference), binding)
                })
                .collect(),
        }
    }

    /// Lookup only: hierarchy selection and signature projection finished before publication.
    pub fn get(&self, module: &str, reference: &KlibDeclarationSignature) -> KlibReferenceOrigin {
        self.references
            .get(&(module.to_owned(), reference.clone()))
            .cloned()
            .unwrap_or(KlibReferenceOrigin::Missing)
    }
}

pub(super) fn normalize_reference(
    libraries: &KlibLibraries,
    module: &str,
    reference: &KlibDeclarationSignature,
) -> KlibReferenceOrigin {
    // A real declaration remains itself. Only an explicitly serialized fake override uses its
    // overridden-symbol edges; missing synthesized declarations use the shared core hierarchy.
    let edges = match libraries.bodies.callee_origin(module, reference) {
        KlibCalleeOrigin::Exact(identity) => {
            let Some((_, function)) = libraries.bodies.function(&identity) else {
                return KlibReferenceOrigin::Missing;
            };
            if function.base.origin != "FAKE_OVERRIDE" {
                return KlibReferenceOrigin::Exact(KlibReferenceBinding {
                    declaration: identity,
                    dispatch: None,
                });
            }
            if function
                .overridden
                .iter()
                .any(|symbol| symbol.kind != KlibIrSymbolKind::Function)
            {
                return KlibReferenceOrigin::Missing;
            }
            Some(
                function
                    .overridden
                    .iter()
                    .map(|symbol| symbol.signature.clone())
                    .collect::<HashSet<_>>(),
            )
        }
        KlibCalleeOrigin::Ambiguous => return KlibReferenceOrigin::Ambiguous,
        KlibCalleeOrigin::Missing => None,
    };
    let KlibDeclarationSignature::Public(signature) = reference else {
        return KlibReferenceOrigin::Missing;
    };
    let [owner_segment, name] = signature.declaration().segments() else {
        // Nested or parameterized reference containers need their own checked scope projection.
        return KlibReferenceOrigin::Missing;
    };
    let reachable = libraries.bodies.body_provider_modules(module);
    let Some((owner, classifier)) =
        libraries
            .inventory
            .all_classifiers()
            .find(|(_, classifier)| {
                classifier.signature.package() == signature.package()
                    && classifier.signature.declaration().segments()
                        == &signature.declaration().segments()[..1]
                    && classifier
                        .module
                        .as_ref()
                        .is_some_and(|module| reachable.contains(module))
            })
    else {
        return KlibReferenceOrigin::Missing;
    };
    if !classifier.shape.type_params().is_empty() {
        return KlibReferenceOrigin::Missing;
    }
    let receiver = Ty::obj_name(owner);
    let candidates =
        crate::symbol_resolver::member_declarations_in_hierarchy(libraries, receiver, name);
    let mut bound = None;
    for candidate in candidates.functions() {
        let Some(realization) = candidate
            .callable
            .external_identity
            .and_then(|id| libraries.external_callable(id))
        else {
            continue;
        };
        let Some(identity) = realization.dependency_callable.as_ref() else {
            continue;
        };
        if edges.as_ref().is_some_and(|edges| {
            let KlibDeclarationSignature::Public(declaration) = &identity.declaration else {
                return true;
            };
            !edges.contains(&KlibIrSignature::Public(declaration.clone()))
        }) {
            continue;
        }
        let Some((
            crate::libraries::CallablePlacement::Member {
                owner: declaring_owner,
                ..
            },
            signed,
        )) = libraries.inventory.dependency_function(identity)
        else {
            continue;
        };
        if owner == declaring_owner {
            continue;
        }
        let declaration = &signed.declaration;
        let result = realization
            .callable
            .generic_sig
            .as_ref()
            .map_or(realization.callable.ret, |generic| generic.ret);
        if !declaration.params.is_empty()
            || declaration.receiver.is_some()
            || declaration.context_count != 0
            || !declaration.formals.is_empty()
            || declaration.is_static
            || declaration.is_suspend
            || result.mentions_ty_param()
        {
            continue;
        }
        let classes = [MetadataClass {
            name: owner_segment,
            type_params: &[],
            expect: classifier.expect,
        }];
        let Ok(projected) = package_function_signature(
            MetadataContainer {
                package: classifier.signature.package().segments(),
                classes: &classes,
                native_interop_library: classifier.native_interop_library,
            },
            declaration,
        ) else {
            continue;
        };
        if projected != *signature {
            continue;
        }
        match libraries
            .bodies
            .callee_origin(module, &identity.declaration)
        {
            KlibCalleeOrigin::Ambiguous => return KlibReferenceOrigin::Ambiguous,
            KlibCalleeOrigin::Exact(canonical) if canonical == *identity => {}
            KlibCalleeOrigin::Missing | KlibCalleeOrigin::Exact(_) => continue,
        }
        // Core member specialization already carries this exact applied hierarchy rung. Do not
        // walk the hierarchy again and risk choosing another application of the same parent.
        let Some(declaring_receiver) = candidate
            .receiver
            .filter(|applied| applied.kotlin_class_internal() == Some(declaring_owner))
        else {
            continue;
        };
        let binding = KlibReferenceBinding {
            declaration: identity.clone(),
            dispatch: Some(KlibReferenceDispatch {
                receiver,
                declaring_receiver,
            }),
        };
        if bound.as_ref().is_some_and(|previous| previous != &binding) {
            return KlibReferenceOrigin::Ambiguous;
        }
        bound = Some(binding);
    }
    bound.map_or(KlibReferenceOrigin::Missing, KlibReferenceOrigin::Exact)
}
