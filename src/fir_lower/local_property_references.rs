//! The reflected property a LOCAL delegated property's conventions receive.
//!
//! kotlinc keeps it with the member and top-level delegated properties of the class lexically
//! declaring it (`PropertyReferenceLowering`), numbered `<v#N>` among that class's local delegated
//! properties in source order. This module records those checked facts; the backend chooses the
//! reference's storage.

use crate::fir::DeclarationId;
use crate::ir::IrLocalPropertyReference;
use crate::types::Ty;

use super::{module_declarations, BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    pub(super) fn local_property_reference(
        &self,
        (name, property_type): (&str, Ty),
        (mutable, ordinal): (bool, u32),
    ) -> Result<IrLocalPropertyReference, FirLoweringFailure> {
        let class = match self.body.lexical_class_owner() {
            Some(owner) => {
                let class = self
                    .ir
                    .checked_classifier_classes
                    .get(&owner)
                    .or_else(|| self.ir.checked_enum_entry_classes.get(&owner))
                    .ok_or(FirLoweringFailure::MissingLocalClass(owner))?;
                Some(self.ir.classes[*class as usize].fq_name_id())
            }
            None => None,
        };
        let member = DeclarationId::from_raw(self.body.owner().raw());
        let missing = || FirLoweringFailure::MissingDeclarationSource(member);
        let source = module_declarations::source(self.index, member).map_err(|_| missing())?;
        let member_order = self.index.source_order(member).ok_or_else(missing)?;
        Ok(IrLocalPropertyReference {
            name: name.into(),
            property_type,
            mutable,
            class,
            source,
            ordinal,
            member_order,
        })
    }
}
