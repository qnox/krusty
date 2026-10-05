//! Whether an enum constant has a body.
//!
//! A constant with a body is an instance of its own subclass of the enum, and that subclass's
//! constructor is where the constant's arguments execute. A constant without one is the enum
//! itself, constructed in the enum's initializer with its arguments evaluated there.

use super::{DeclarationId, ResolvedModuleIndex};

impl ResolvedModuleIndex {
    /// Whether the enum entry `entry` declares a body: any declaration it owns.
    pub(crate) fn enum_entry_has_body(&self, entry: DeclarationId) -> bool {
        (0..self.declaration_count()).any(|raw| {
            let child = DeclarationId::from_raw(
                u32::try_from(raw).expect("too many stable declarations for a packed id"),
            );
            self.declaration_anchor(child)
                .is_some_and(|anchor| anchor.owner == Some(entry))
        })
    }
}
