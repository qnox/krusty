//! The branch shapes of checked `try` and `when` expressions: each catch clause and each `when` arm
//! with the conditions it tests.

use crate::fir::header::{FirExprId, LocalValueId, OriginId};
use crate::fir::signature::ResolvedTy;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirCatch {
    pub origin: OriginId,
    pub parameter: LocalValueId,
    pub parameter_ty: ResolvedTy,
    pub body: FirExprId,
    /// The `catch` keyword's source line, a line-only output fact; 0 when unknown.
    pub debug_line: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirWhenBranch {
    pub origin: OriginId,
    pub conditions: Box<[FirWhenCondition]>,
    pub guard: Option<FirExprId>,
    pub result: FirExprId,
}

/// A `when` condition after the checker has distinguished value patterns from predicates that
/// already consume the subject (`is`/`!is` and `in`/`!in`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirWhenCondition {
    /// `candidate` compared with the subject. `subject_ty` is the subject's type for this
    /// comparison after earlier conditions' false-branch smart casts, when that type is a numeric
    /// primitive different from the subject's declared type. `None` compares the declared type.
    SubjectEquals {
        candidate: FirExprId,
        subject_ty: Option<ResolvedTy>,
    },
    Predicate(FirExprId),
}
