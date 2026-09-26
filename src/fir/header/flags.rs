//! Bit-packed parser-owned facts carried by one compact header node.
//!
//! Each of these is a copy of what the declaration WROTE, not a semantic conclusion: a resolver
//! that needs one reads it here rather than reconstructing it from a resolved type, which cannot
//! distinguish a modifier from the shape it happens to sit on.

use crate::ast::TypeRef;

/// Every bit of parser-owned type syntax which affects semantic type resolution. This is a compact
/// copy, not a semantic type: it exists only while explicit headers are resolved and is discarded
/// before checked FIR streaming starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeFlags(u8);

impl HeaderTypeFlags {
    const NULLABLE: u8 = 1 << 0;
    const DEFINITELY_NON_NULL: u8 = 1 << 1;
    const FUNCTION_RECEIVER: u8 = 1 << 2;
    const SUSPEND_FUNCTION: u8 = 1 << 3;
    const IN_PROJECTION: u8 = 1 << 4;
    const OUT_PROJECTION: u8 = 1 << 5;
    const IMPORT: u8 = 1 << 6;
    const STAR_PROJECTION: u8 = 1 << 7;

    pub(super) fn from_type_ref(ty: &TypeRef) -> Self {
        let mut bits = 0;
        for (flag, enabled) in [
            (Self::NULLABLE, ty.nullable()),
            (Self::DEFINITELY_NON_NULL, ty.definitely_non_null()),
            (Self::FUNCTION_RECEIVER, ty.fun_has_receiver()),
            (Self::SUSPEND_FUNCTION, ty.fun_suspend()),
            (Self::IN_PROJECTION, ty.in_projection()),
            (Self::OUT_PROJECTION, ty.out_projection()),
            (Self::IMPORT, ty.is_import()),
            (Self::STAR_PROJECTION, ty.is_star_projection()),
        ] {
            if enabled {
                bits |= flag;
            }
        }
        Self(bits)
    }

    pub const fn nullable(self) -> bool {
        self.0 & Self::NULLABLE != 0
    }

    pub const fn definitely_non_null(self) -> bool {
        self.0 & Self::DEFINITELY_NON_NULL != 0
    }

    pub const fn function_receiver(self) -> bool {
        self.0 & Self::FUNCTION_RECEIVER != 0
    }

    pub const fn suspend_function(self) -> bool {
        self.0 & Self::SUSPEND_FUNCTION != 0
    }

    pub const fn in_projection(self) -> bool {
        self.0 & Self::IN_PROJECTION != 0
    }

    pub const fn out_projection(self) -> bool {
        self.0 & Self::OUT_PROJECTION != 0
    }

    pub const fn is_import(self) -> bool {
        self.0 & Self::IMPORT != 0
    }

    pub const fn star_projection(self) -> bool {
        self.0 & Self::STAR_PROJECTION != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderParameterFlags(u8);

impl HeaderParameterFlags {
    pub(super) const VARARG: u8 = 1 << 0;
    pub(super) const DEFAULT: u8 = 1 << 1;
    pub(super) const PROPERTY: u8 = 1 << 2;
    pub(super) const MUTABLE_PROPERTY: u8 = 1 << 3;
    pub(super) const MATERIALIZED_LAMBDA: u8 = 1 << 4;
    pub(super) const CROSSINLINE: u8 = 1 << 5;

    /// The modifiers a source value parameter wrote.
    pub(super) const fn of_value_parameter(parameter: &crate::ast::Param) -> Self {
        Self(0)
            .with(Self::VARARG, parameter.is_vararg)
            .with(Self::DEFAULT, parameter.default.is_some())
            .with(Self::MATERIALIZED_LAMBDA, parameter.is_materialized_lambda)
            .with(Self::CROSSINLINE, parameter.is_crossinline)
    }

    pub(super) const fn with(mut self, flag: u8, enabled: bool) -> Self {
        if enabled {
            self.0 |= flag;
        }
        self
    }

    pub const fn is_vararg(self) -> bool {
        self.0 & Self::VARARG != 0
    }

    pub const fn has_default(self) -> bool {
        self.0 & Self::DEFAULT != 0
    }

    pub const fn is_property(self) -> bool {
        self.0 & Self::PROPERTY != 0
    }

    pub const fn is_mutable_property(self) -> bool {
        self.0 & Self::MUTABLE_PROPERTY != 0
    }

    /// The parameter wrote `noinline`: its argument is a real closure with a local of its own,
    /// where an ordinary inline function parameter is spliced at each use and owns no local. Both
    /// parameters are function-TYPED, so only this modifier separates them.
    pub const fn materializes_its_lambda(self) -> bool {
        self.0 & Self::MATERIALIZED_LAMBDA != 0
    }

    /// The parameter wrote `crossinline`.
    pub const fn is_crossinline(self) -> bool {
        self.0 & Self::CROSSINLINE != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeaderTypeParameterFlags(u8);

impl HeaderTypeParameterFlags {
    const IN: u8 = 1 << 0;
    const OUT: u8 = 1 << 1;
    const NON_NULL: u8 = 1 << 2;
    const REIFIED: u8 = 1 << 3;

    pub(super) fn new(variance: crate::types::TypeVariance, non_null: bool, reified: bool) -> Self {
        let mut flags = Self::default();
        flags.0 |= match variance {
            crate::types::TypeVariance::Invariant => 0,
            crate::types::TypeVariance::In => Self::IN,
            crate::types::TypeVariance::Out => Self::OUT,
        };
        if non_null {
            flags.0 |= Self::NON_NULL;
        }
        if reified {
            flags.0 |= Self::REIFIED;
        }
        flags
    }

    pub(crate) fn from_semantics(
        variance: crate::types::TypeVariance,
        non_null: bool,
        reified: bool,
    ) -> Self {
        Self::new(variance, non_null, reified)
    }

    pub const fn is_in(self) -> bool {
        self.0 & Self::IN != 0
    }

    pub const fn is_out(self) -> bool {
        self.0 & Self::OUT != 0
    }

    pub const fn is_non_null(self) -> bool {
        self.0 & Self::NON_NULL != 0
    }

    pub const fn is_reified(self) -> bool {
        self.0 & Self::REIFIED != 0
    }
}
