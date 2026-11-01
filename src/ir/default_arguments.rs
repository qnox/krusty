//! Checked default-provider relationships retained for backend-neutral consumers.

use super::Callee;

/// The stable current-module identity, when an override target belongs to the source module.
fn module_override_callable(
    target: crate::fir::ResolvedFunctionOverrideTarget,
) -> Option<crate::fir::CallableId> {
    match target {
        crate::fir::ResolvedFunctionOverrideTarget::Module(callable) => Some(callable),
        crate::fir::ResolvedFunctionOverrideTarget::External(_) => None,
    }
}

impl Callee {
    /// A same-module call whose selected declaration inherits defaults from another declaration.
    /// Both values are stable checked identities; target consumers need not inspect FIR variants.
    pub(crate) fn inherited_module_default_provider(
        &self,
    ) -> Option<(crate::fir::CallableId, crate::fir::CallableId)> {
        let Self::ModuleWithDefaults {
            target,
            default_provider,
            ..
        } = self
        else {
            return None;
        };
        let provider = module_override_callable(*default_provider)?;
        (*target != provider).then_some((*target, provider))
    }
}
