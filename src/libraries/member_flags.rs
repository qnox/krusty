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
    /// A signature-polymorphic method (JLS 15.12.3): its call's descriptor comes from the call
    /// site's argument types.
    pub(super) const SIGNATURE_POLYMORPHIC: u16 = 1 << 12;

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
    pub const fn with_signature_polymorphic(self, on: bool) -> Self {
        self.with(Self::SIGNATURE_POLYMORPHIC, on)
    }
    #[inline]
    pub const fn with_is_classifier_static(self, on: bool) -> Self {
        self.with(Self::IS_CLASSIFIER_STATIC, on)
    }
}

/// The member accessors over [`LmFlags`], kept with the flag set they read and write.
impl super::LibraryMember {
    #[inline]
    pub fn ret_nullable(&self) -> bool {
        self.flags.has(LmFlags::RET_NULLABLE)
    }
    #[inline]
    pub fn is_interface(&self) -> bool {
        self.flags.has(LmFlags::IS_INTERFACE)
    }
    #[inline]
    pub fn suspend(&self) -> bool {
        self.flags.has(LmFlags::SUSPEND)
    }
    #[inline]
    pub fn is_operator(&self) -> bool {
        self.flags.has(LmFlags::IS_OPERATOR)
    }
    #[inline]
    pub fn is_infix(&self) -> bool {
        self.flags.has(LmFlags::IS_INFIX)
    }
    #[inline]
    pub fn is_member_extension(&self) -> bool {
        self.flags.has(LmFlags::IS_EXTENSION)
    }
    #[inline]
    pub fn is_primary_constructor(&self) -> bool {
        self.flags.has(LmFlags::IS_PRIMARY_CONSTRUCTOR)
    }
    #[inline]
    pub fn deprecated_hidden(&self) -> bool {
        self.flags.has(LmFlags::DEPRECATED_HIDDEN)
    }

    /// Whether the declaration is signature-polymorphic (JLS 15.12.3).
    #[inline]
    pub fn is_signature_polymorphic(&self) -> bool {
        self.flags.has(LmFlags::SIGNATURE_POLYMORPHIC)
    }
    #[inline]
    pub fn set_signature_polymorphic(&mut self, on: bool) {
        self.flags = self.flags.with_signature_polymorphic(on);
    }

    pub fn is_classifier_static(&self) -> bool {
        self.flags.has(LmFlags::IS_CLASSIFIER_STATIC)
    }
    #[inline]
    pub fn set_ret_nullable(&mut self, on: bool) {
        self.flags = self.flags.with_ret_nullable(on);
    }
    #[inline]
    pub fn set_is_primary_constructor(&mut self, on: bool) {
        self.flags = self.flags.with_is_primary_constructor(on);
    }
    #[inline]
    pub fn set_deprecated_hidden(&mut self, on: bool) {
        self.flags = self.flags.with_deprecated_hidden(on);
    }

    pub fn set_is_classifier_static(&mut self, on: bool) {
        self.flags = self.flags.with_is_classifier_static(on);
    }
    #[inline]
    pub fn set_is_interface(&mut self, on: bool) {
        self.flags = self.flags.with_is_interface(on);
    }
    #[inline]
    pub fn set_suspend(&mut self, on: bool) {
        self.flags = self.flags.with_suspend(on);
    }
    pub fn set_is_operator(&mut self, on: bool) {
        self.flags = self.flags.with_is_operator(on);
    }
    pub fn set_is_infix(&mut self, on: bool) {
        self.flags = self.flags.with_is_infix(on);
    }
    pub fn set_is_member_extension(&mut self, on: bool) {
        self.flags = self.flags.with_is_extension(on);
    }
    pub fn is_abstract(&self) -> bool {
        self.flags.has(LmFlags::IS_ABSTRACT)
    }
    pub fn set_is_abstract(&mut self, on: bool) {
        self.flags = self.flags.with_is_abstract(on);
    }
    pub fn is_final(&self) -> bool {
        self.flags.has(LmFlags::IS_FINAL)
    }
    pub fn set_is_final(&mut self, on: bool) {
        self.flags = self.flags.with_is_final(on);
    }
    /// A class delegating an interface that declares this member does not forward it: the
    /// member's own implementation stays inherited.
    pub fn inherited_by_delegation(&self) -> bool {
        self.flags.has(LmFlags::INHERITED_BY_DELEGATION)
    }
    pub fn set_inherited_by_delegation(&mut self, on: bool) {
        self.flags = self.flags.with_inherited_by_delegation(on);
    }
}
