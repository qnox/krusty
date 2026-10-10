//! Provider identities of published declarations, interned by their exact `IdSignature`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::fir::ExternalCallableId;
use crate::libraries::{
    ExternalCallableKind, ExternalCallableRealization, FnKind, FunctionInfo,
    FunctionParameterIdentities, KlibDeclarationSignature,
};
use crate::metadata::id_signature::KlibPublicIdSignature;

/// One identity per exact declaration signature. The signature, not a spelling or a parameter
/// tuple, decides whether two candidates are the same declaration.
#[derive(Default)]
pub(super) struct ExternalIdentities {
    realizations: RefCell<Vec<ExternalCallableRealization>>,
    by_signature: RefCell<HashMap<KlibPublicIdSignature, ExternalCallableId>>,
}

impl ExternalIdentities {
    /// Give a normalized top-level function the identity of its declaration, publishing the
    /// declaration's realization the first time its signature is seen.
    pub(super) fn assign_function(
        &self,
        signature: &KlibPublicIdSignature,
        parameters: &FunctionParameterIdentities,
        function: &mut FunctionInfo,
    ) {
        if let Some(identity) = self.by_signature.borrow().get(signature).copied() {
            function.callable.external_identity = Some(identity);
            return;
        }
        let mut realizations = self.realizations.borrow_mut();
        let identity = ExternalCallableId::from_raw(
            u32::try_from(realizations.len())
                .expect("too many external callable declarations for packed FIR identity"),
        );
        function.callable.external_identity = Some(identity);
        let kind = match function.kind {
            FnKind::TopLevel => ExternalCallableKind::TopLevel,
            FnKind::Extension => ExternalCallableKind::Extension,
            FnKind::Member => ExternalCallableKind::Member,
        };
        realizations.push(ExternalCallableRealization {
            callable: function.callable.clone(),
            kind,
            declaration_owner: function.callable.declaration_owner,
            parameter_identities: parameters.physical.clone(),
            declaration_signature: Some(KlibDeclarationSignature::Public(signature.clone())),
        });
        self.by_signature
            .borrow_mut()
            .insert(signature.clone(), identity);
    }

    pub(super) fn realization(
        &self,
        identity: ExternalCallableId,
    ) -> Option<ExternalCallableRealization> {
        self.realizations
            .borrow()
            .get(identity.raw() as usize)
            .cloned()
    }
}
