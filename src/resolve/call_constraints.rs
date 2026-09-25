//! Receiver-call constraints shared by extension selection and rejected-call diagnostics.

use super::*;

/// Facts fixed by a receiver call independently of its value arguments. Rejected generic
/// candidates also use these constraints when rendering their specialized parameter types.
#[derive(Clone, Copy)]
pub(super) struct CallConstraints<'a> {
    pub(super) type_args: &'a [Ty],
    pub(super) receiver: Ty,
    pub(super) expected: Option<Ty>,
}

impl CallConstraints<'_> {
    pub(super) fn bindings(
        self,
        signature: &crate::libraries::GenericSig,
    ) -> crate::symbol_resolver::GSigBinds {
        let mut bindings = crate::symbol_resolver::seeded_gsig_binds(signature, self.type_args);
        if !self.type_args.is_empty() {
            return bindings;
        }
        let mut seeded = crate::symbol_resolver::GSigBinds::new();
        if let Some(declared) = signature.receiver {
            crate::symbol_resolver::unify_ty(declared, self.receiver, &mut seeded);
        }
        if let Some(expected) = self.expected {
            crate::symbol_resolver::unify_ty(signature.ret, expected, &mut seeded);
        }
        for (formal, ty) in seeded {
            if signature.formals.contains(&formal) && !ty.mentions_pending() && ty != Ty::Error {
                bindings.entry(formal).or_insert(ty);
            }
        }
        bindings
    }
}
