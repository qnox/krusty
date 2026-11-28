//! Property identity lookups, and the `companion { … }` block properties each classifier declares.
//!
//! A block property is hoisted out of its classifier's body, so it is not among that classifier's
//! owned declarations. Its checked receiver is the classifier that declared the block; this
//! records it there once, when the property's identity is published, so a later phase asks for a
//! classifier's block properties by resolved identity instead of scanning the module.

use super::*;

/// The block properties of each classifier, in publication order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct CompanionBlockProperties {
    by_classifier: HashMap<TypeName, Vec<DeclarationId>>,
}

impl ResolvedModuleIndex {
    pub fn property_declaration(&self, property: PropertyId) -> Option<DeclarationId> {
        self.property(property).map(|property| property.declaration)
    }

    pub fn property(&self, property: PropertyId) -> Option<ResolvedPropertyHeader> {
        self.properties.get(&property).copied()
    }

    pub fn property_for_declaration(&self, declaration: DeclarationId) -> Option<PropertyId> {
        self.property_by_declaration.get(&declaration).copied()
    }

    /// The block properties `classifier` declares, by declaration.
    pub fn companion_block_properties(&self, classifier: TypeName) -> &[DeclarationId] {
        self.companion_block_properties
            .by_classifier
            .get(&classifier)
            .map_or(&[], Vec::as_slice)
    }

    /// Record `declaration` among its classifier's block properties when it is one: its header
    /// says so, and its checked receiver is the classifier that declared the block.
    pub(super) fn record_companion_block_property(
        &mut self,
        declaration: DeclarationId,
        receiver: Option<ResolvedTy>,
    ) {
        let is_block_member = self
            .declaration_header(declaration)
            .is_some_and(|header| header.flags.has(DeclarationFlags::COMPANION_BLOCK_MEMBER));
        if !is_block_member {
            return;
        }
        let classifier = receiver
            .and_then(|receiver| receiver.get().non_null().obj_internal())
            .expect("a companion-block property's receiver is its declaring classifier");
        self.companion_block_properties
            .by_classifier
            .entry(classifier)
            .or_default()
            .push(declaration);
    }
}
