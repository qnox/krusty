//! Singleton members exposed to post-resolution plugin expression planning.
//!
//! A plugin plan may call a member of a classifier's singleton — its companion object, or the
//! classifier itself when it is an `object` — that no source expression names. kotlinx's
//! `serializer<T>()` intrinsic does exactly that: it calls `T.Companion.serializer(…)`. The plugin
//! decides WHICH declaration is that accessor from its full declared signature; this boundary
//! only publishes the candidates, each with its stable identity and its declaration-owned
//! parameter and result types. Plugins never look a member up themselves, and no later phase
//! re-selects one by name or arity.

use std::collections::{HashMap, HashSet};

use crate::fir::{DeclarationFlags, DeclarationKind, ResolvedModuleIndex};
use crate::libraries::{SemanticPlatform, TypeKind};
use crate::plugins::{FrontendCallableTarget, FrontendDeclaredCallable, FrontendSingletonMembers};
use crate::symbol_source::SymbolSource;
use crate::types::TypeName;

use super::plugin_expression_annotations::ClassifierAnnotationInputs;

/// The singleton members named `names` of every classifier in `named`, and each classifier's own
/// type-parameter count. A current-module classifier is read from the resolved module index, a
/// dependency classifier from its normalized provider; a classifier neither knows is absent.
pub(super) fn singleton_members_for_classifiers(
    inputs: ClassifierAnnotationInputs<'_>,
    named: &HashSet<TypeName>,
    names: &[&str],
) -> (
    HashMap<TypeName, FrontendSingletonMembers>,
    HashMap<TypeName, usize>,
) {
    let mut members = HashMap::new();
    let mut type_parameters = HashMap::new();
    for &classifier in named {
        if let Some((index, declaration)) = inputs.resolved_index.and_then(|index| {
            index
                .classifier_declaration(classifier)
                .map(|declaration| (index, declaration))
        }) {
            type_parameters.insert(classifier, module_type_parameter_count(index, declaration));
            if let Some(singleton) = module_singleton_members(index, declaration, classifier, names)
            {
                members.insert(classifier, singleton);
            }
            continue;
        }
        let Some((count, singleton)) =
            dependency_singleton_members(inputs.libraries, classifier, names)
        else {
            continue;
        };
        type_parameters.insert(classifier, count);
        if let Some(singleton) = singleton {
            members.insert(classifier, singleton);
        }
    }
    (members, type_parameters)
}

fn module_type_parameter_count(
    index: &ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
) -> usize {
    (0u32..)
        .take_while(|&ordinal| index.type_parameter(declaration, ordinal).is_some())
        .count()
}

fn module_singleton_members(
    index: &ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    classifier: TypeName,
    names: &[&str],
) -> Option<FrontendSingletonMembers> {
    let header = index.declaration_header(declaration)?;
    let is_object = header.flags.has(DeclarationFlags::SINGLETON)
        && !header.flags.has(DeclarationFlags::COMPANION);
    let (receiver_declaration, receiver) = if is_object {
        (declaration, classifier)
    } else {
        let companion = index.companion_declaration(declaration)?;
        let receiver = index.classifier_identity(companion).or_else(|| {
            index
                .classifier_header(companion)
                .map(|header| header.classifier)
        })?;
        (companion, receiver)
    };
    let callables = index
        .owned_declarations(receiver_declaration)
        .iter()
        .filter_map(|&owned| {
            let header = index.declaration_header(owned)?;
            if header.kind != DeclarationKind::Function {
                return None;
            }
            let callable = index.callable_for_declaration(owned)?;
            let name = index.callable_name(callable.id)?;
            if !names.contains(&name) {
                return None;
            }
            let signature = index.signature(owned)?;
            Some(FrontendDeclaredCallable {
                name: name.to_owned(),
                params: signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                ret: signature.result.get(),
                target: FrontendCallableTarget::Module(callable.id),
            })
        })
        .collect();
    Some(FrontendSingletonMembers {
        receiver,
        receiver_is_classifier: is_object,
        callables,
    })
}

fn dependency_singleton_members(
    libraries: &dyn SemanticPlatform,
    classifier: TypeName,
    names: &[&str],
) -> Option<(usize, Option<FrontendSingletonMembers>)> {
    let shape = SymbolSource::classifier(libraries, classifier)?;
    let count = shape.own_type_parameter_count;
    let is_object = shape.kind == TypeKind::Object;
    let receiver = if is_object {
        Some(classifier)
    } else {
        shape
            .companion_object
            .as_ref()
            .map(|(_, companion)| *companion)
    };
    let Some(receiver) = receiver else {
        return Some((count, None));
    };
    // The singleton's own declared members, as the provider records them for the classifier:
    // instance members of the companion or object, with their dependency identities.
    let singleton = SymbolSource::classifier(libraries, receiver)?;
    let callables = names
        .iter()
        .filter_map(|&name| singleton.declared_callables.get(name))
        .flat_map(|callables| {
            callables
                .functions()
                .iter()
                .filter_map(|function| {
                    let callable = &function.callable;
                    // The metadata-primary generic signature is the declaration's semantic one;
                    // `params`/`ret` alone are its erased physical shape.
                    let (params, ret) = match callable.generic_sig.as_deref() {
                        Some(signature) => (signature.params.clone(), signature.ret),
                        None => (callable.params.clone(), callable.ret),
                    };
                    Some(FrontendDeclaredCallable {
                        name: callable.name.clone(),
                        params,
                        ret,
                        target: FrontendCallableTarget::External(callable.external_identity?),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect();
    Some((
        count,
        Some(FrontendSingletonMembers {
            receiver,
            receiver_is_classifier: is_object,
            callables,
        }),
    ))
}
