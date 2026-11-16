//! Select dependency SAM declarations before backend entry, retaining exact producer authority.

use crate::ir::{CheckedKlibSamDeclaration, IrSamMethod, IrSamTarget, IrSamTypeParameter};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::KlibDependencyCallable;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::symbol_resolver::{semantic_sam_signature, SamMethodDeclaration};
use crate::symbol_source::SymbolSource;

pub(super) fn freeze(
    source: &dyn SymbolSource,
    bodies: &KlibDeclarationBodies,
    module: &str,
    signature: &KlibPublicIdSignature,
    name: crate::types::TypeName,
) -> Option<(CheckedKlibSamDeclaration, Option<KlibDependencyCallable>)> {
    let classifier = source.classifier(name)?;
    let identity = classifier.klib_identity.as_ref()?;
    let producers = bodies.body_provider_modules(module);
    if identity.signature != *signature || !producers.contains(&identity.module) {
        return None;
    }
    if !matches!(
        bodies.public_class(module, signature),
        crate::klib_libraries::LocatedClass::One(_)
    ) || bodies
        .class_header(&identity.module, signature)
        .ok()
        .flatten()
        .is_none()
    {
        return None;
    }
    let selected = semantic_sam_signature(source, classifier.declared_self_type(name))?;
    let (method_target, method) = match selected.declaration? {
        SamMethodDeclaration::External(external) => {
            let realization = source.external_callable(external)?;
            if realization.callable.external_identity != Some(external)
                || realization.callable.params != selected.declared_params
                || realization.callable.ret != selected.declared_ret
            {
                return None;
            }
            let method = realization.dependency_callable?;
            if !producers.contains(&method.module) {
                return None;
            }
            (IrSamMethod::External(external), Some(method))
        }
        SamMethodDeclaration::FunctionTypeInvoke => (IrSamMethod::FunctionTypeInvoke, None),
        SamMethodDeclaration::Module(_) => return None,
    };
    let target = IrSamTarget {
        classifier: name,
        method: selected.method,
        method_target,
        parameters: selected.params,
        contravariant_parameters: selected.contravariant_params,
        result: selected.ret,
        declared_parameters: selected.declared_params,
        declared_result: selected.declared_ret,
        context_count: u32::try_from(selected.context_count).ok()?,
        has_receiver: selected.has_receiver,
        suspend: selected.suspend,
        source_suspend: false,
        overridden_results: selected.overridden_results,
        function_adapter: false,
        wraps_function_value: true,
        nullable: false,
        kotlin_interface: selected.kotlin_interface,
        parameter_identities: selected.parameter_identities.into_vec(),
        type_parameters: selected
            .type_parameters
            .into_iter()
            .map(|p| IrSamTypeParameter {
                name: p.name,
                bounds: p.bounds,
            })
            .collect(),
    };
    Some((CheckedKlibSamDeclaration { classifier, target }, method))
}
