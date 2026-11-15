//! Root allocation consumes the exact constructor declaration, never a class spelling alone.

use super::*;

impl Body<'_> {
    pub(in super::super) fn root_any_construction(
        &mut self,
        target: ExternalCallableId,
        internal: TypeName,
        args: &[u32],
        selected: Option<&[Ty]>,
    ) -> Option<Result<(), Unsupported>> {
        let fact = self.callables.callable(target)?;
        let owner = krusty::types::wk::any();
        if fact.declaration_owner != Some(krusty::types::SemanticCallableOwner::Classifier(owner))
            || fact.kind != krusty::libraries::ExternalCallableKind::Constructor
        {
            return None;
        }
        if internal != owner
            || !args.is_empty()
            || selected != Some(&[])
            || !fact.params.is_empty()
            || !fact.parameter_identities.is_empty()
            || fact.ret != Ty::Unit
            || fact.context_count != 0
            || fact.source_receiver.is_some()
            || fact.generic_sig.is_some()
            || fact.declares_parameter_defaults
            || fact.owner_is_interface
        {
            return Some(Err(
                "a root Any constructor with a mismatched declaration frame".into(),
            ));
        }
        let table = match self.runtime.root_any_table(self.module, self.classifiers) {
            Ok(table) => table,
            Err(error) => return Some(Err(error)),
        };
        self.code
            .global_get(table)
            .call(self.runtime.identity.allocate)
            .struct_new(self.runtime.identity.object);
        Some(Ok(()))
    }
}
