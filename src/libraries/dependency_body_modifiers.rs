//! Declaration-owned implementation modifiers carried to serialized KLIB body authentication.

use crate::metadata::semantic::{
    KotlinAccessorModifiers, KotlinFunction, KotlinModality, KotlinProperty,
};

/// Known metadata facts, distinct from the absence of a body declaration contract.
///
/// Property external status is independent of accessor external status. It vetoes ordinary body
/// emission without changing the accessor's own modifier word or the serialized function flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KlibBodyDeclarationModifiers {
    pub(crate) is_inline: bool,
    pub(crate) is_suspend: bool,
    pub(crate) is_external: bool,
    pub(crate) property_is_external: bool,
    pub(crate) modality: KotlinModality,
}

impl KlibBodyDeclarationModifiers {
    pub(crate) fn function(declaration: &KotlinFunction) -> Self {
        Self {
            is_inline: declaration.is_inline,
            is_suspend: declaration.is_suspend,
            is_external: declaration.is_external,
            property_is_external: false,
            modality: declaration.modality,
        }
    }

    pub(crate) fn accessor(property: &KotlinProperty, accessor: &KotlinAccessorModifiers) -> Self {
        Self {
            is_inline: accessor.is_inline,
            is_suspend: false,
            is_external: accessor.is_external,
            property_is_external: property.is_external,
            modality: accessor.modality,
        }
    }
}
