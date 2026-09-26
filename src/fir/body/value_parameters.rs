//! Checked value parameters of one FIR body: their bindings, retained defaults, and `vararg`.

use super::{FirBody, FirExprId, LocalValueId, OriginId, ResolvedTy};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirValueParameter {
    pub origin: OriginId,
    pub value: LocalValueId,
    pub ty: ResolvedTy,
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
