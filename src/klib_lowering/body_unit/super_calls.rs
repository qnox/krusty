//! `super` calls of an interface default: the member runs without dispatch, so a unit that emits
//! the default for the call itself declares it as a method no class table numbers.

use super::*;

impl Linker<'_, '_> {
    /// The member `callee` a `super` call runs. It links as any call of the member does, except
    /// that an interface body the closure froze as emitted for a `super` call is declared in the
    /// unit and run directly.
    pub(super) fn link_super_call(
        &mut self,
        ir: &mut IrFile,
        callee: &KlibPublicIdSignature,
    ) -> Result<LinkedCallee, KlibBodyDeclineReason> {
        let identity = self.callee_identity(callee)?;
        let Some(KlibCalleeFact::Selected(callables)) = self.sources.callees.get(&identity) else {
            return self.link(ir, callee);
        };
        if !self.sources.callees.is_object_default(&identity)
            || self.sources.externals.contains_key(&identity)
        {
            return self.link(ir, callee);
        }
        let selected = callables[0];
        let (arena, declaration) = self.sources.bodies.function(&identity).ok_or_else(|| {
            KlibBodyDeclineReason::CalleeDeclined(Box::new(KlibBodyDecline::new(
                KlibDeclarationSignature::Public(callee.clone()),
                KlibBodyDeclineReason::Unjoined,
            )))
        })?;
        self.value_member(ir, &identity, selected, arena, declaration)
    }
}
