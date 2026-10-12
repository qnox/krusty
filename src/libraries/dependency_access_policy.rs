//! Friend-module access facts owned by a dependency provider.
//!
//! Classifier ownership is sufficient for providers whose physical owner names one module. KLIB
//! packages can merge declarations from several archives, so callable access additionally uses the
//! provider-assigned stable identity selected during resolution.

use crate::fir::ExternalCallableId;
use crate::types::TypeName;

/// Exact friend-module access policy for compiled declarations.
pub trait DependencyAccessPolicy {
    /// Whether this classifier belongs to a friend module of the current compilation.
    fn internal_classifier_accessible(&self, _owner: TypeName) -> bool {
        false
    }

    /// Whether this internal callable belongs to a friend module of the current compilation.
    fn internal_callable_accessible(
        &self,
        _identity: Option<ExternalCallableId>,
        owner: TypeName,
    ) -> bool {
        self.internal_classifier_accessible(owner)
    }
}

struct ClosedDependencyAccess;

impl DependencyAccessPolicy for ClosedDependencyAccess {}

static CLOSED_DEPENDENCY_ACCESS: ClosedDependencyAccess = ClosedDependencyAccess;

pub(super) fn closed_dependency_access() -> &'static dyn DependencyAccessPolicy {
    &CLOSED_DEPENDENCY_ACCESS
}
