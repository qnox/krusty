//! The non-public classes of its own library that a private declaration's signature names.
//!
//! Kotlin lets a declaration private to a class or file expose a class private to the same file,
//! as a parameter, receiver, bound or result. Such a class has no public signature, so the
//! declaration's header names it as its library's bodies name it
//! ([`super::same_library_class::classifier`]), and a body that constructs or reads the class
//! installs it as it installs any class of its own library.

use super::decline::KlibBodyDeclineReason;
use super::klib_types::KlibTypeParameters;
use crate::klib_libraries::KlibDeclarationBodies;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrFunction, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};

/// Name in `types` every class of `module` without a public signature that the header of
/// `function` names. Any other non-public classifier stays unnamed, and reading it still declines.
pub(super) fn bind(
    types: &mut KlibTypeParameters,
    bodies: &KlibDeclarationBodies,
    module: &str,
    arena: &KlibIrArena,
    function: &KlibIrFunction,
) -> Result<(), KlibBodyDeclineReason> {
    let mut pending = header_types(function);
    let mut named = crate::fx_hash::HashSet::default();
    while let Some(id) = pending.pop() {
        let (classifier, arguments) = match arena.ty(id) {
            KlibIrType::Simple {
                classifier,
                arguments,
                ..
            } => (classifier, arguments),
            KlibIrType::DefinitelyNotNull(inner) => {
                pending.push(*inner);
                continue;
            }
            KlibIrType::Dynamic { .. } | KlibIrType::Error { .. } => continue,
        };
        pending.extend(arguments.iter().filter_map(|argument| match argument {
            KlibIrTypeArgument::Type { ty, .. } => Some(*ty),
            KlibIrTypeArgument::Star => None,
        }));
        let signature = &classifier.signature;
        if classifier.kind != KlibIrSymbolKind::Class
            || matches!(signature, KlibIrSignature::Public(_))
            || !named.insert(signature.clone())
        {
            continue;
        }
        if let Some(name) = super::same_library_class::classifier(bodies, module, signature)? {
            types.bind_classifier(signature, name);
        }
    }
    Ok(())
}

/// The types `function`'s header declares: its receivers, parameters, result and type-parameter
/// bounds.
fn header_types(function: &KlibIrFunction) -> Vec<KlibIrTypeId> {
    let parameters = function
        .dispatch_receiver
        .iter()
        .chain(&function.context_parameters)
        .chain(&function.extension_receiver)
        .chain(&function.regular_parameters);
    parameters
        .flat_map(|parameter| std::iter::once(parameter.ty).chain(parameter.vararg_element_type))
        .chain(std::iter::once(function.return_type))
        .chain(
            function
                .type_parameters
                .iter()
                .flat_map(|parameter| parameter.supertypes.iter().copied()),
        )
        .collect()
}
