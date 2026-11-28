//! The constant string arguments of one annotation application on a KLIB IR declaration.

use crate::metadata::klib_ir::tree::{KlibIrArena, KlibIrArguments, KlibIrExprKind};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSignature, KlibIrSymbolKind};

/// The one string argument of the one application of the annotation whose constructor is
/// `package`/`declaration`.
pub(super) fn ir_string_annotation<'a>(
    arena: &'a KlibIrArena,
    annotations: &'a [crate::metadata::klib_ir::tree::KlibIrAnnotation],
    package: &[&str],
    declaration: &[&str],
) -> Option<&'a str> {
    match ir_string_annotation_arguments(arena, annotations, package, declaration)?.as_slice() {
        [Some(value)] => Some(value),
        _ => None,
    }
}

/// The arguments of the one application of the annotation whose constructor is
/// `package`/`declaration`, when each is a string constant or left to its default (`None`).
pub(super) fn ir_string_annotation_arguments<'a>(
    arena: &'a KlibIrArena,
    annotations: &'a [crate::metadata::klib_ir::tree::KlibIrAnnotation],
    package: &[&str],
    declaration: &[&str],
) -> Option<Vec<Option<&'a str>>> {
    let mut matches = annotations.iter().filter(|annotation| {
        if annotation.access.symbol.kind != KlibIrSymbolKind::Constructor
            || annotation.constructor_type_arguments != 0
            || !annotation.access.type_arguments.is_empty()
        {
            return false;
        }
        let KlibIrSignature::Public(signature) = &annotation.access.symbol.signature else {
            return false;
        };
        signature
            .package()
            .segments()
            .iter()
            .map(String::as_str)
            .eq(package.iter().copied())
            && signature
                .declaration()
                .segments()
                .iter()
                .map(String::as_str)
                .eq(declaration.iter().copied())
    });
    let annotation = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let arguments = match &annotation.access.arguments {
        KlibIrArguments::Flat(arguments) => arguments.as_slice(),
        KlibIrArguments::Split {
            dispatch_receiver: None,
            extension_receiver: None,
            values,
        } => values.as_slice(),
        KlibIrArguments::Split { .. } => return None,
    };
    arguments
        .iter()
        .map(|argument| match argument {
            None => Some(None),
            Some(argument) => match &arena.expr(*argument).kind {
                KlibIrExprKind::Const(KlibIrConstant::String(value)) => Some(Some(value.as_str())),
                _ => None,
            },
        })
        .collect()
}
