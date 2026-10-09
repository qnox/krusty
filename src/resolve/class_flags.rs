//! Declaration-level classifier modifiers, packed once per source classifier.
//!
//! The same packed value seeds the all-files header index and the complete `ClassSig`, so early
//! inference and final checking never classify one declaration differently.

use crate::ast::ClassDecl;

/// Bit-packed boolean modifiers for a [`ClassSig`](super::ClassSig). Each per-class flag below
/// would cost a full byte as a separate `bool` field (plus struct padding); collapsed into one `u16`
/// they save several bytes per sig, and the compiler builds a few thousand. Built with the `with_*`
/// chain from [`ClassFlags::default`]; read through the `ClassSig::is_*` / `has_*` accessors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassFlags(u16);

impl ClassFlags {
    pub(super) const INTERFACE: u16 = 1 << 0;
    pub(super) const OBJECT: u16 = 1 << 1;
    pub(super) const ABSTRACT: u16 = 1 << 2;
    pub(super) const FUN_INTERFACE: u16 = 1 << 3;
    pub(super) const SEALED: u16 = 1 << 4;
    pub(super) const FINAL: u16 = 1 << 5;
    pub(super) const HAS_ABSTRACT_MEMBERS: u16 = 1 << 6;
    pub(super) const ANNOTATION: u16 = 1 << 7;
    /// `data class`: positional destructuring reads its primary-constructor properties.
    pub(super) const DATA: u16 = 1 << 8;

    #[inline]
    const fn with(mut self, mask: u16, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    pub(super) const fn has(self, mask: u16) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_interface(self, on: bool) -> Self {
        self.with(Self::INTERFACE, on)
    }
    #[inline]
    pub const fn with_object(self, on: bool) -> Self {
        self.with(Self::OBJECT, on)
    }
    #[inline]
    pub const fn with_abstract(self, on: bool) -> Self {
        self.with(Self::ABSTRACT, on)
    }
    #[inline]
    pub const fn with_fun_interface(self, on: bool) -> Self {
        self.with(Self::FUN_INTERFACE, on)
    }
    #[inline]
    pub const fn with_sealed(self, on: bool) -> Self {
        self.with(Self::SEALED, on)
    }
    #[inline]
    pub const fn with_final(self, on: bool) -> Self {
        self.with(Self::FINAL, on)
    }
    #[inline]
    pub const fn with_has_abstract_members(self, on: bool) -> Self {
        self.with(Self::HAS_ABSTRACT_MEMBERS, on)
    }
    #[inline]
    pub const fn with_annotation(self, on: bool) -> Self {
        self.with(Self::ANNOTATION, on)
    }
    #[inline]
    pub const fn with_data(self, on: bool) -> Self {
        self.with(Self::DATA, on)
    }
}

/// One source of truth for declaration-level classifier flags. The same packed value seeds the
/// all-files header index and is later installed on the complete `ClassSig`, preventing early
/// inference and final checking from classifying a declaration differently.
pub(super) fn source_class_flags(class: &ClassDecl) -> ClassFlags {
    ClassFlags::default()
        .with_interface(class.is_interface())
        .with_object(class.is_singleton())
        .with_abstract(class.is_abstract())
        .with_fun_interface(class.is_fun_interface)
        .with_sealed(class.is_sealed())
        .with_final(class.is_final())
        .with_has_abstract_members(
            class.methods.iter().any(|method| method.is_abstract())
                || class.body_props.iter().any(|property| property.is_abstract),
        )
        .with_annotation(class.is_annotation())
        .with_data(class.is_data)
}

/// Classifier flags read from the compact Pass-1 declaration inventory.
///
/// `HAS_ABSTRACT_MEMBERS` is a property of the classifier's direct declaration children, not of
/// its parser container. Computing it from stable ownership lets production bootstrap source
/// classifiers after the corresponding `ClassDecl` has been destroyed.
pub(super) fn streamed_source_class_flags(
    headers: &crate::fir::StreamedHeaderModule,
    classifier: &crate::fir::DeclarationStub,
) -> ClassFlags {
    let flags = classifier.flags;
    let has_abstract_members = headers.stubs.iter().any(|candidate| {
        candidate.flags.has(crate::fir::DeclarationFlags::ABSTRACT)
            && matches!(
                candidate.kind,
                crate::fir::DeclarationKind::Function
                    | crate::fir::DeclarationKind::Property
                    | crate::fir::DeclarationKind::Accessor
            )
            && headers
                .declarations
                .anchor(candidate.id)
                .is_some_and(|anchor| anchor.owner == Some(classifier.id))
    });
    ClassFlags::default()
        .with_interface(flags.has(crate::fir::DeclarationFlags::INTERFACE))
        .with_object(flags.has(crate::fir::DeclarationFlags::SINGLETON))
        .with_abstract(flags.has(crate::fir::DeclarationFlags::ABSTRACT))
        .with_fun_interface(flags.has(crate::fir::DeclarationFlags::FUN_INTERFACE))
        .with_sealed(flags.has(crate::fir::DeclarationFlags::SEALED))
        .with_final(flags.has(crate::fir::DeclarationFlags::FINAL))
        .with_has_abstract_members(has_abstract_members)
        .with_annotation(flags.has(crate::fir::DeclarationFlags::ANNOTATION_CLASS))
        .with_data(flags.has(crate::fir::DeclarationFlags::DATA))
}
