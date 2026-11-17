//! Select dependency SAM declarations before backend entry, retaining exact producer authority.

use crate::ir::{CheckedKlibSamDeclaration, IrSamMethod, IrSamTarget, IrSamTypeParameter};
use crate::klib_libraries::KlibDeclarationBodies;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::symbol_resolver::{semantic_sam_signature, SamMethodDeclaration};
use crate::symbol_source::SymbolSource;

pub(super) fn freeze(
    source: &dyn SymbolSource,
    bodies: &KlibDeclarationBodies,
    module: &str,
    signature: &KlibPublicIdSignature,
    name: crate::types::TypeName,
) -> Option<CheckedKlibSamDeclaration> {
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
    let method_target = match selected.declaration? {
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
            IrSamMethod::External(external)
        }
        SamMethodDeclaration::FunctionTypeInvoke => IrSamMethod::FunctionTypeInvoke,
        SamMethodDeclaration::Module(_) => return None,
    };
    let frames = |declarations: &[crate::symbol_resolver::SamMethodFrame]| {
        declarations
            .iter()
            .map(|frame| {
                let method = match frame.declaration {
                    Some(SamMethodDeclaration::External(external)) => {
                        let realized = source.external_callable(external)?;
                        if !producers.contains(&realized.dependency_callable.as_ref()?.module)
                            || realized.callable.external_identity != Some(external)
                            || realized.callable.params != frame.parameters
                            || realized.callable.ret != frame.result
                        {
                            return None;
                        }
                        Some(IrSamMethod::External(external))
                    }
                    Some(SamMethodDeclaration::FunctionTypeInvoke) => {
                        Some(IrSamMethod::FunctionTypeInvoke)
                    }
                    Some(SamMethodDeclaration::Module(_)) => return None,
                    None => None,
                };
                Some(crate::ir::IrSamMethodFrame {
                    method,
                    parameters: frame.parameters.clone(),
                    result: frame.result,
                })
            })
            .collect::<Option<Vec<_>>>()
    };
    let dispatch_methods = frames(&selected.dispatch_methods)?;
    let default_methods = frames(&selected.default_methods)?;
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
        requires_object_dispatch: selected.requires_object_dispatch,
        overridden_results: selected.overridden_results,
        dispatch_methods,
        default_methods,
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
    Some(CheckedKlibSamDeclaration {
        classifier,
        target,
        apply: crate::symbol_resolver::apply_sam_declaration,
    })
}
