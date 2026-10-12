//! Compact declaration flags carried by provider-normalized library members.

/// Bit-packed boolean flags for a library member. Read through the member accessors of the same
/// names; mutate through their matching `set_*` methods or this type's `with_*` builders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LmFlags(u16);

impl LmFlags {
    pub(super) const RET_NULLABLE: u16 = 1 << 0;
    pub(super) const IS_INTERFACE: u16 = 1 << 1;
    pub(super) const SUSPEND: u16 = 1 << 2;
    /// The member is declared `operator`. Only Kotlin metadata records this source fact.
    pub(super) const IS_OPERATOR: u16 = 1 << 3;
    /// A member extension whose declaring class supplies its dispatch receiver.
    pub(super) const IS_EXTENSION: u16 = 1 << 4;
    pub(super) const IS_ABSTRACT: u16 = 1 << 5;
    pub(super) const IS_INFIX: u16 = 1 << 6;
    pub(super) const IS_FINAL: u16 = 1 << 7;
    /// Delegation keeps this implemented member's inherited body instead of forwarding it.
    pub(super) const INHERITED_BY_DELEGATION: u16 = 1 << 8;
    /// This constructor is the classifier's primary declaration.
    pub(super) const IS_PRIMARY_CONSTRUCTOR: u16 = 1 << 9;
    /// Retained for override matching and binary compatibility, but hidden from ordinary calls.
    pub(super) const DEPRECATED_HIDDEN: u16 = 1 << 10;
    /// A receiver-less platform declaration published in a classifier's static namespace.
    pub(super) const IS_CLASSIFIER_STATIC: u16 = 1 << 11;

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
    pub const fn with_ret_nullable(self, on: bool) -> Self {
        self.with(Self::RET_NULLABLE, on)
    }
    #[inline]
    pub const fn with_is_interface(self, on: bool) -> Self {
        self.with(Self::IS_INTERFACE, on)
    }
    #[inline]
    pub const fn with_suspend(self, on: bool) -> Self {
        self.with(Self::SUSPEND, on)
    }
    #[inline]
    pub const fn with_is_operator(self, on: bool) -> Self {
        self.with(Self::IS_OPERATOR, on)
    }
    #[inline]
    pub const fn with_is_extension(self, on: bool) -> Self {
        self.with(Self::IS_EXTENSION, on)
    }
    #[inline]
    pub const fn with_is_abstract(self, on: bool) -> Self {
        self.with(Self::IS_ABSTRACT, on)
    }
    #[inline]
    pub const fn with_is_final(self, on: bool) -> Self {
        self.with(Self::IS_FINAL, on)
    }
    #[inline]
    pub const fn with_is_infix(self, on: bool) -> Self {
        self.with(Self::IS_INFIX, on)
    }
    #[inline]
    pub const fn with_inherited_by_delegation(self, on: bool) -> Self {
        self.with(Self::INHERITED_BY_DELEGATION, on)
    }
    #[inline]
    pub const fn with_is_primary_constructor(self, on: bool) -> Self {
        self.with(Self::IS_PRIMARY_CONSTRUCTOR, on)
    }
    #[inline]
    pub const fn with_deprecated_hidden(self, on: bool) -> Self {
        self.with(Self::DEPRECATED_HIDDEN, on)
    }
    #[inline]
    pub const fn with_is_classifier_static(self, on: bool) -> Self {
        self.with(Self::IS_CLASSIFIER_STATIC, on)
    }
}
