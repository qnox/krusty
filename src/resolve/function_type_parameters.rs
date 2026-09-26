//! Which callables take a value of a function type: FIR's `isSomeFunctionType` over a resolved
//! signature's value parameters and extension receiver, decided here against the classifier
//! declarations so later phases consume the published fact instead of classifying types.

use super::SymbolTable;
use crate::fir::{CallableId, DeclarationId, ResolvedCallableHeader, ResolvedModuleIndex};
use crate::module_symbols::ModuleSymbols;
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::{type_name, Ty, KFUNCTION_INTERNAL};

/// Publish the fact for every module callable once its Pass-1 signature is final.
pub(crate) fn publish_function_type_parameters(
    index: &mut ResolvedModuleIndex,
    table: &SymbolTable,
) {
    let callables = {
        let module = ModuleSymbols::new(table);
        let source =
            CompositeSource::new(vec![&module as &dyn SymbolSource, table.libraries.as_ref()]);
        function_typed(index, &source, |_| true)
    };
    for callable in callables {
        index.publish_function_typed_parameter(callable);
    }
}

/// The same fact for the members of body-local classifiers, whose signatures Pass 2 publishes.
pub(crate) fn publish_local_function_type_parameters(
    index: &mut ResolvedModuleIndex,
    platform: &dyn crate::libraries::SemanticPlatform,
    source_file: u32,
    classifiers: &[DeclarationId],
) {
    let callables = {
        let module = crate::fir::StreamedModuleSymbols::for_file(index, source_file);
        let source = CompositeSource::new(vec![
            &module as &dyn SymbolSource,
            platform as &dyn SymbolSource,
        ]);
        function_typed(index, &source, |header| {
            index
                .declaration_anchor(header.declaration)
                .and_then(|anchor| anchor.owner)
                .is_some_and(|owner| classifiers.contains(&owner))
        })
    };
    for callable in callables {
        index.publish_function_typed_parameter(callable);
    }
}

fn function_typed(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    selected: impl Fn(&ResolvedCallableHeader) -> bool,
) -> Vec<CallableId> {
    index
        .callable_headers()
        .filter(|header| selected(header))
        .filter(|header| {
            let Some(signature) = index.signature(header.declaration) else {
                return false;
            };
            signature
                .parameters
                .iter()
                .skip(header.shape.context_parameter_count as usize)
                .map(|parameter| parameter.get())
                .chain(
                    header
                        .shape
                        .extension_receiver
                        .map(|receiver| receiver.get()),
                )
                .any(|ty| is_some_function_type(source, ty))
        })
        .map(|header| header.id)
        .collect()
}

/// FIR's `isSomeFunctionType`: arrow syntax, a function-type classifier, or its reflective
/// counterpart (`KFunctionN`, `KSuspendFunctionN`), which the provider normalizes with the callable
/// shape beside a direct `kotlin.reflect.KFunction` supertype.
fn is_some_function_type(source: &dyn SymbolSource, ty: Ty) -> bool {
    let ty = ty.non_null();
    matches!(ty, Ty::Fun(_))
        || ty
            .kotlin_class_internal()
            .and_then(|internal| source.classifier(internal))
            .is_some_and(|classifier| {
                classifier.represents_function_type()
                    || (classifier.callable_signature.is_some()
                        && classifier
                            .supertypes
                            .contains_name(type_name(KFUNCTION_INTERNAL)))
            })
}
