//! Declaration-owned implementation modifiers carried to serialized KLIB body authentication.

use crate::metadata::semantic::{
    KotlinAccessorModifiers, KotlinFunction, KotlinModality, KotlinProperty,
};

/// Known metadata facts, distinct from the absence of a body declaration contract.
///
/// Property external status is independent of accessor external status. It vetoes ordinary body
/// emission without changing the accessor's own modifier word or the serialized function flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KlibBodyDeclarationModifiers {
    pub(crate) is_inline: bool,
    /// The declaration has a `reified` type parameter, whose run-time type only an expansion at
    /// each call site supplies.
    pub(crate) has_reified_type_parameters: bool,
    pub(crate) is_suspend: bool,
    pub(crate) is_external: bool,
    pub(crate) property_is_external: bool,
    pub(crate) modality: KotlinModality,
}

impl KlibBodyDeclarationModifiers {
    /// Whether the selected member can dispatch to a subclass implementation.
    pub fn is_overridable(self) -> bool {
        matches!(
            self.modality,
            KotlinModality::Open | KotlinModality::Abstract
        )
    }

    /// Whether the selected semantic declaration requires a continuation frame.
    pub fn is_suspend(self) -> bool {
        self.is_suspend
    }

    /// Whether a body unit lowers the declaration from its serialized body as an inline function
    /// whose every call it expands, as kotlinc expands a call of one in a library it links.
    pub(crate) fn permits_expanded_body(self) -> bool {
        self.is_inline
            && !self.is_external
            && !self.property_is_external
            && self.modality == KotlinModality::Final
    }

    /// Whether a call of the declaration runs a body the support unit lowers from its serialized
    /// one. A suspend body is one too: the common coroutine preparation gives it the continuation
    /// ABI and the state machine, as it does a source suspend function.
    ///
    /// An `inline` declaration qualifies unless it has a `reified` type parameter: a call that
    /// is not expanded at its site (a delegated property's convention call, a callable reference)
    /// runs the same body as an ordinary call does.
    pub(crate) fn permits_ordinary_body(self) -> bool {
        !(self.is_inline && self.has_reified_type_parameters)
            && !self.is_external
            && !self.property_is_external
            && matches!(self.modality, KotlinModality::Final | KotlinModality::Open)
    }

    /// Whether the declaration is an abstract member implementation: a call of it always reaches
    /// an override, which the receiver's class supplies.
    pub(crate) fn declares_abstract_implementation(self) -> bool {
        !self.is_external && !self.property_is_external && self.modality.is_abstract()
    }

    /// Whether the declaration is abstract: a call of it runs the override the receiver's class
    /// supplies, never a body of its own.
    pub fn is_abstract(self) -> bool {
        self.modality.is_abstract()
    }

    pub(crate) fn function(declaration: &KotlinFunction) -> Self {
        Self {
            is_inline: declaration.is_inline,
            has_reified_type_parameters: declaration.has_reified_type_params,
            is_suspend: declaration.is_suspend,
            is_external: declaration.is_external,
            property_is_external: false,
            modality: declaration.modality,
        }
    }

    pub(crate) fn accessor(property: &KotlinProperty, accessor: &KotlinAccessorModifiers) -> Self {
        Self {
            is_inline: accessor.is_inline,
            has_reified_type_parameters: property.formals.iter().any(|formal| formal.reified),
            is_suspend: false,
            is_external: accessor.is_external,
            property_is_external: property.is_external,
            modality: accessor.modality,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_member_dispatch_requires_an_overridable_declaration() {
        for (modality, expected) in [
            (KotlinModality::Final, false),
            (KotlinModality::Open, true),
            (KotlinModality::Abstract, true),
            (KotlinModality::Sealed, false),
        ] {
            let modifiers = KlibBodyDeclarationModifiers {
                is_inline: false,
                has_reified_type_parameters: false,
                is_suspend: false,
                is_external: false,
                property_is_external: false,
                modality,
            };
            assert_eq!(modifiers.is_overridable(), expected, "{modality:?}");
        }
    }

    #[test]
    fn ordinary_body_admission_requires_every_implementation_modifier() {
        let ordinary = KlibBodyDeclarationModifiers {
            is_inline: false,
            has_reified_type_parameters: false,
            is_suspend: false,
            is_external: false,
            property_is_external: false,
            modality: KotlinModality::Final,
        };
        assert!(ordinary.permits_ordinary_body());
        assert!(KlibBodyDeclarationModifiers {
            modality: KotlinModality::Open,
            ..ordinary
        }
        .permits_ordinary_body());
        assert!(KlibBodyDeclarationModifiers {
            is_suspend: true,
            ..ordinary
        }
        .permits_ordinary_body());
        assert!(KlibBodyDeclarationModifiers {
            is_inline: true,
            ..ordinary
        }
        .permits_ordinary_body());
        for excluded in [
            KlibBodyDeclarationModifiers {
                is_inline: true,
                has_reified_type_parameters: true,
                ..ordinary
            },
            KlibBodyDeclarationModifiers {
                is_external: true,
                ..ordinary
            },
            KlibBodyDeclarationModifiers {
                property_is_external: true,
                ..ordinary
            },
            KlibBodyDeclarationModifiers {
                modality: KotlinModality::Abstract,
                ..ordinary
            },
            KlibBodyDeclarationModifiers {
                modality: KotlinModality::Sealed,
                ..ordinary
            },
        ] {
            assert!(!excluded.permits_ordinary_body(), "{excluded:?}");
        }
    }
}
