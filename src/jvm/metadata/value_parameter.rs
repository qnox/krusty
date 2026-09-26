//! A value parameter as Kotlin metadata declares it: its modifiers and type facts, packed into
//! flags the declaration's call shape is read from.

use crate::types::InlineParameterModifier;
use crate::types::TypeName;

/// Bit-packed boolean flags for a [`MetaValueParam`], collapsing its `has_default`/`crossinline`/
/// `noinline`/`vararg`/`recv_fun`/`nullable`/`suspend_fun`/`has_type_facts`/`no_infer` bytes into one. Read through
/// the `MetaValueParam` accessors of the same names; built with the `with_*` chain. Headroom for
/// their accessors below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MvpFlags(u16);

impl MvpFlags {
    const HAS_DEFAULT: u16 = 1 << 0;
    const CROSSINLINE: u16 = 1 << 1;
    const VARARG: u16 = 1 << 2;
    const RECV_FUN: u16 = 1 << 3;
    const NULLABLE: u16 = 1 << 4;
    const SUSPEND_FUN: u16 = 1 << 5;
    const HAS_TYPE_FACTS: u16 = 1 << 6;
    const NO_INFER: u16 = 1 << 7;
    const NOINLINE: u16 = 1 << 8;

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
    const fn has(self, mask: u16) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_has_default(self, on: bool) -> Self {
        self.with(Self::HAS_DEFAULT, on)
    }
    #[inline]
    pub const fn with_inline_modifier(self, inlining: InlineParameterModifier) -> Self {
        self.with(
            Self::CROSSINLINE,
            matches!(inlining, InlineParameterModifier::Crossinline),
        )
        .with(
            Self::NOINLINE,
            matches!(inlining, InlineParameterModifier::Noinline),
        )
    }
    #[inline]
    pub const fn with_vararg(self, on: bool) -> Self {
        self.with(Self::VARARG, on)
    }
    #[inline]
    pub const fn with_recv_fun(self, on: bool) -> Self {
        self.with(Self::RECV_FUN, on)
    }
    #[inline]
    pub const fn with_nullable(self, on: bool) -> Self {
        self.with(Self::NULLABLE, on)
    }
    #[inline]
    pub const fn with_suspend_fun(self, on: bool) -> Self {
        self.with(Self::SUSPEND_FUN, on)
    }
    #[inline]
    pub const fn with_has_type_facts(self, on: bool) -> Self {
        self.with(Self::HAS_TYPE_FACTS, on)
    }

    #[inline]
    pub const fn with_no_infer(self, on: bool) -> Self {
        self.with(Self::NO_INFER, on)
    }
}

#[derive(Clone, Debug)]
pub struct MetaValueParam {
    pub ty: Option<TypeName>,
    pub name: String,
    /// Bit-packed `has_default`/`crossinline`/`noinline`/`vararg`/`recv_fun`/`nullable`/`suspend_fun` (read
    /// via the accessors below).
    /// `vararg` — `vararg elem: T`. Only `@Metadata` records this: the JVM descriptor shows just the
    /// packed array, so `f(vararg c: Char)` and `f(c: CharArray)` are indistinguishable without it,
    /// and overload resolution cannot know it may spread trailing arguments into the array.
    pub flags: MvpFlags,
    pub recv_fun_receiver: Option<TypeName>,
}

impl MetaValueParam {
    #[inline]
    pub fn has_default(&self) -> bool {
        self.flags.has(MvpFlags::HAS_DEFAULT)
    }
    #[inline]
    pub fn inline_modifier(&self) -> InlineParameterModifier {
        if self.flags.has(MvpFlags::NOINLINE) {
            InlineParameterModifier::Noinline
        } else if self.flags.has(MvpFlags::CROSSINLINE) {
            InlineParameterModifier::Crossinline
        } else {
            InlineParameterModifier::None
        }
    }
    #[inline]
    pub fn vararg(&self) -> bool {
        self.flags.has(MvpFlags::VARARG)
    }
    #[inline]
    pub fn nullable(&self) -> bool {
        self.flags.has(MvpFlags::NULLABLE)
    }
    #[inline]
    pub fn recv_fun(&self) -> bool {
        self.flags.has(MvpFlags::RECV_FUN)
    }
    /// The parameter's declared type is a `suspend` FUNCTION TYPE (`suspend Scope.(Req) -> Resp`) —
    /// metadata's `Type.flags` SUSPEND_TYPE bit, the only witness that the CPS-erased
    /// `FunctionN+1<…, Continuation<T>, Any?>` shape is a suspend function type and not a
    /// source-level continuation-taking one.
    #[inline]
    pub fn suspend_fun(&self) -> bool {
        self.flags.has(MvpFlags::SUSPEND_FUN)
    }
    /// Whether the parameter's declared `Type` was resolved, either inline or through its enclosing
    /// type table. If neither representation can be read, type-level facts are absent rather than
    /// false, and a consumer must not treat them as disclaimers.
    #[inline]
    pub fn has_type_facts(&self) -> bool {
        self.flags.has(MvpFlags::HAS_TYPE_FACTS)
    }
    #[inline]
    pub fn no_infer(&self) -> bool {
        self.flags.has(MvpFlags::NO_INFER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(flags: u64) -> InlineParameterModifier {
        MetaValueParam {
            ty: None,
            name: "block".to_string(),
            flags: MvpFlags::default()
                .with_has_default(true)
                .with_inline_modifier(
                    crate::metadata::DeclaredValueParameter::of_flags(flags).inline_modifier,
                )
                .with_vararg(true),
            recv_fun_receiver: None,
        }
        .inline_modifier()
    }

    /// `ValueParameter.flags`: bit 1 declares a default, bit 2 is `crossinline`, bit 3 `noinline`.
    #[test]
    fn crossinline_and_noinline_are_published_apart() {
        assert_eq!(published(0), InlineParameterModifier::None);
        assert_eq!(published(1 << 1), InlineParameterModifier::None);
        assert_eq!(published(1 << 2), InlineParameterModifier::Crossinline);
        assert_eq!(published(1 << 3), InlineParameterModifier::Noinline);
        assert_eq!(
            published((1 << 1) | (1 << 2)),
            InlineParameterModifier::Crossinline
        );
    }

    #[test]
    fn the_modifier_leaves_the_other_flags_alone() {
        let flags = MvpFlags::default()
            .with_has_default(true)
            .with_inline_modifier(InlineParameterModifier::Noinline)
            .with_vararg(true)
            .with_no_infer(true)
            .with_inline_modifier(InlineParameterModifier::Crossinline);
        let parameter = MetaValueParam {
            ty: None,
            name: "block".to_string(),
            flags,
            recv_fun_receiver: None,
        };
        assert_eq!(
            parameter.inline_modifier(),
            InlineParameterModifier::Crossinline
        );
        assert!(parameter.has_default());
        assert!(parameter.vararg());
        assert!(parameter.no_infer());
        assert!(!parameter.nullable());
    }
}
