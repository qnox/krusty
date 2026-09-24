//! Checked default-provider relationships retained for backend-neutral consumers.

use super::Callee;

impl Callee {
    /// A same-module call whose selected declaration inherits defaults from another declaration.
    /// Both values are stable checked identities; target consumers need not inspect FIR variants.
    pub(crate) fn inherited_module_default_provider(
        &self,
    ) -> Option<(crate::fir::CallableId, crate::fir::CallableId)> {
        let Self::ModuleWithDefaults {
            target,
            default_provider: crate::fir::ResolvedFunctionOverrideTarget::Module(provider),
            ..
        } = self
        else {
            return None;
        };
        (*target != *provider).then_some((*target, *provider))
    }
}
