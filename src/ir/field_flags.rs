//! The boolean facts of an [`IrField`](super::IrField), packed into one byte.

/// Bit-packed boolean flags for an [`IrField`], collapsing `has_default`/`is_final`/`is_private`/
/// `is_lateinit` into one byte. Read through the `IrField` accessors of the same names; built with the
/// `with_*` chain. Headroom for four more flags before the byte fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrfFlags(u8);

impl IrfFlags {
    pub(super) const HAS_DEFAULT: u8 = 1 << 0;
    pub(super) const IS_FINAL: u8 = 1 << 1;
    pub(super) const IS_PRIVATE: u8 = 1 << 2;
    pub(super) const IS_LATEINIT: u8 = 1 << 3;
    /// Storage the compiler introduces with no source declaration behind it, such as an interface
    /// delegation's `$$delegate_N`.
    pub(super) const COMPILER_GENERATED: u8 = 1 << 4;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    pub(super) const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_has_default(self, on: bool) -> Self {
        self.with(Self::HAS_DEFAULT, on)
    }
    #[inline]
    pub const fn with_is_final(self, on: bool) -> Self {
        self.with(Self::IS_FINAL, on)
    }
    #[inline]
    pub const fn with_is_private(self, on: bool) -> Self {
        self.with(Self::IS_PRIVATE, on)
    }
    #[inline]
    pub const fn with_is_lateinit(self, on: bool) -> Self {
        self.with(Self::IS_LATEINIT, on)
    }
    #[inline]
    pub const fn with_compiler_generated(self, on: bool) -> Self {
        self.with(Self::COMPILER_GENERATED, on)
    }
}

impl super::IrField {
    /// See [`IrfFlags::COMPILER_GENERATED`].
    #[inline]
    pub fn is_compiler_generated(&self) -> bool {
        self.flags.has(IrfFlags::COMPILER_GENERATED)
    }
    #[inline]
    pub fn with_compiler_generated(mut self, on: bool) -> Self {
        self.flags = self.flags.with_compiler_generated(on);
        self
    }
}
