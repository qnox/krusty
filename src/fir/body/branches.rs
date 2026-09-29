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

/// Numeric adaptations selected for a subject-form equality after earlier conditions have
/// smart-cast the subject. Every field is a checked semantic type; lowering only materializes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirWhenSubjectNumericEquality {
    pub subject_unbox: ResolvedTy,
    pub subject_widening: Option<ResolvedTy>,
    pub candidate_widening: Option<ResolvedTy>,
}

/// A `when` condition after the checker has distinguished value patterns from predicates that
/// already consume the subject (`is`/`!is` and `in`/`!in`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirWhenCondition {
    /// `candidate` compared with the subject. `numeric` is the complete adaptation plan selected
    /// after earlier conditions' false-branch smart casts; `None` keeps ordinary structural
    /// equality at the subject's declared type.
    SubjectEquals {
        candidate: FirExprId,
        numeric: Option<FirWhenSubjectNumericEquality>,
    },
    Predicate(FirExprId),
}
