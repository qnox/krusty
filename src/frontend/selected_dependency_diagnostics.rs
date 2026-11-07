//! Presentation of the exact dependency declaration retained by the live frontend provider.

use crate::fir::ExternalCallableId;
use crate::libraries::SemanticPlatform;

/// Render an already-selected declaration for a failure diagnostic. This reads only the opaque
/// identity's retained realization; it never resolves a name or tries another callable.
pub(crate) fn selected_external_call_diagnostic(
    platform: &dyn SemanticPlatform,
    target: ExternalCallableId,
) -> Option<String> {
    let selected = platform.external_callable(target)?;
    Some(format!(
        "selected dependency owner={:?}, name={:?}, declaration={:?}, dependency={:?}",
        selected.declaration_owner,
        selected.callable.name,
        selected.declaration_signature,
        selected.dependency_callable,
    ))
}
