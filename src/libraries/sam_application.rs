//! Pure specialization of a selected SAM shape through normalized classifier application facts.

use super::classifier_bindings;
use crate::types::{ty_subst_keep_unbound, Ty, TyKind, TyParamBindings as GSigBinds};

pub(crate) fn sam_application_bindings(
    classifier: &crate::libraries::LibraryType,
    applied: Ty,
) -> (GSigBinds, GSigBinds) {
    let mut logical_bindings = classifier_bindings(classifier, applied);
    // `in String` checks the converted function at `String`, while its capture-read view is
    // `Any?`. Keep that distinction only as provenance; `out` and `*` have no such divergence.
    let capture_read_bindings = logical_bindings
        .iter()
        .map(|(name, argument)| {
            (
                crate::types::TyParamName::from(name),
                contravariant_capture_read(*argument),
            )
        })
        .collect::<GSigBinds>();
    for argument in logical_bindings.values_mut() {
        // A SAM method receives/returns values, never use-site projection syntax. Composed
        // variance can legitimately leave more than one capture shell here: selecting
        // `Consumer<in T>` through `Holder<out X>` produces `in (out X)`. Consume the complete
        // shell so the abstract method is specialized to the value type `X`, rather than
        // publishing a top-level `out X` parameter into checked FIR.
        while let Some(projected) = argument.projection_inner() {
            *argument = projected;
        }
    }
    (logical_bindings, capture_read_bindings)
}

/// The read view of a use-site capture, used only to identify an `in`-origin parameter. It is not
/// the interface method's callable input: `Sink<in String>` still accepts `String`.
fn contravariant_capture_read(mut argument: Ty) -> Ty {
    while argument.projection_inner().is_some() {
        let read = argument.projection_read_ty();
        if read == argument {
            break;
        }
        argument = read;
    }
    argument
}

pub(crate) fn sam_substitute(
    declared: Ty,
    occurrence_bounds: &crate::types::TyParamBindings,
    bindings: &GSigBinds,
) -> Ty {
    let platform_inner = match declared.kind() {
        TyKind::PlatformNullable(inner) => Some(inner),
        _ => None,
    };
    let explicit_nullable = platform_inner.and_then(|inner| match inner.kind() {
        TyKind::TyParam(name, _) | TyKind::Var(name, _) => bindings
            .get(name.as_str())
            .copied()
            .filter(|binding| matches!(binding.kind(), TyKind::Nullable(_))),
        _ => None,
    });
    if let Some(binding) = explicit_nullable {
        return binding;
    }
    ty_subst_keep_unbound(
        crate::types::ty_with_param_bounds(declared, occurrence_bounds),
        bindings,
    )
}
