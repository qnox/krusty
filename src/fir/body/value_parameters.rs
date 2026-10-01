//! Checked value parameters of one FIR body: their bindings, retained defaults, and `vararg`.

use super::{FirBody, FirExprId, LocalValueId, OriginId, ResolvedTy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirValueParameter {
    pub origin: OriginId,
    pub value: LocalValueId,
    pub ty: ResolvedTy,
    pub name: FirValueParameterName,
    /// `noinline` / `crossinline` as the declaration wrote it. Lowering copies this onto the
    /// parameter's expansion mode; it does not look the modifier up again.
    pub inline_modifier: crate::types::InlineParameterModifier,
}

/// What names a value parameter. Kotlin gives the two parameters that bind no name of their own
/// special names (`<unused var>`, `<destruct>`), which declaration metadata records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirValueParameterName {
    /// The name the parameter binds in its body, source-written or the implicit `it`.
    Bound,
    /// A lambda's `_` parameter.
    Unused,
    /// A lambda parameter written as a destructuring declaration `(a, b)`.
    Destructured,
}

impl FirValueParameter {
    /// A parameter that binds its own name in the body.
    pub fn bound(origin: OriginId, value: LocalValueId, ty: ResolvedTy) -> Self {
        Self {
            origin,
            value,
            ty,
            name: FirValueParameterName::Bound,
            inline_modifier: crate::types::InlineParameterModifier::None,
        }
    }

    pub fn with_inline_modifier(mut self, modifier: crate::types::InlineParameterModifier) -> Self {
        self.inline_modifier = modifier;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirDefaultValue {
    pub origin: OriginId,
    pub parameter: u32,
    /// Checked declaration-boundary type of this default's parameter. Context type receivers are
    /// not runtime value parameters, so their presence makes `parameter` unsuitable as an index
    /// into [`FirBody::parameters`]. Lowering consumes this resolved type directly.
    pub ty: ResolvedTy,
    pub value: FirExprId,
}

/// The declared `vararg` parameter of a local function body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirVarargParameter {
    /// Index among the source value parameters, context parameters excluded.
    pub index: u32,
    /// No value parameter follows it.
    pub is_last: bool,
}

impl FirBody {
    pub fn set_vararg_parameter(&mut self, parameter: FirVarargParameter) {
        assert!(
            self.vararg_parameter.replace(parameter).is_none(),
            "a FIR body declares at most one vararg parameter"
        );
    }

    pub const fn vararg_parameter(&self) -> Option<FirVarargParameter> {
        self.vararg_parameter
    }
}
