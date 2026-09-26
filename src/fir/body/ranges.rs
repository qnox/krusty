//! Checked facts about Kotlin's integral ranges and progressions that counted loops consume.

use crate::types::Ty;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirRangeOperation {
    Through,
    OpenEnd,
    Until,
    DownTo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirRangeCounterKind {
    Int,
    Long,
    Char,
    UInt,
    ULong,
}

impl FirRangeCounterKind {
    pub const fn ty(self) -> Ty {
        match self {
            Self::Int => Ty::Int,
            Self::Long => Ty::Long,
            Self::Char => Ty::Char,
            Self::UInt => Ty::UInt,
            Self::ULong => Ty::ULong,
        }
    }

    pub const fn of(ty: Ty) -> Option<Self> {
        Some(match ty {
            Ty::Int => Self::Int,
            Ty::Long => Self::Long,
            Ty::Char => Self::Char,
            Ty::UInt => Self::UInt,
            Ty::ULong => Self::ULong,
            _ => return None,
        })
    }
}

/// A progression value a counted loop iterates by reading its `first`, `last` and `step`
/// (kotlinc's `DefaultProgressionHandler`), with the members the resolver selected from the
/// progression class's declarations. A `*Range` has step 1 and always increases, so it has no
/// `step` read; any other progression's direction is known only at run time from its step's sign.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirProgressionClass {
    /// The progression's static type, whose members the loop reads.
    pub ty: Ty,
    pub counter: FirRangeCounterKind,
    pub first: super::FirPropertyTarget,
    pub last: super::FirPropertyTarget,
    pub step: Option<super::FirPropertyTarget>,
}

/// A stdlib function a counted loop calls on its own (`getProgressionLastElement`), as resolution
/// selected it from the provider's declarations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirRuntimeFunction {
    pub function: super::ExternalCallableId,
    pub parameters: Box<[Ty]>,
    pub result: Ty,
}

/// Where a counted loop's progression comes from, as kotlinc's `HeaderInfoBuilder` handlers see
/// it. The checker builds this from the selected `kotlin.ranges` declarations; common lowering turns
/// it into the loop's first, last and step.
#[derive(Clone, Debug, PartialEq)]
pub enum FirProgressionSource {
    /// `start..end`, `start..<end`, `start until end` or `start downTo end`.
    Literal {
        operation: FirRangeOperation,
        start: super::FirExprId,
        end: super::FirExprId,
    },
    /// A progression value read through its `first`, `last` and `step`.
    Value {
        progression: Box<FirProgressionClass>,
        iterable: super::FirExprId,
    },
    /// `nested step step`, moving `last` with the `getProgressionLastElement` overload resolution
    /// selected for the progression's class.
    Step {
        nested: Box<FirProgressionSource>,
        step: super::FirExprId,
        last_element: FirRuntimeFunction,
    },
    /// `nested.reversed()`.
    Reversed(Box<FirProgressionSource>),
}

impl FirProgressionSource {
    /// Whether kotlinc can iterate this progression with an inclusive last bound, which `step` and
    /// `reversed` need (`revertToLastInclusive`): only `until` and `..<` have no inclusive form.
    pub fn has_inclusive_last(&self) -> bool {
        !matches!(
            self,
            Self::Literal {
                operation: FirRangeOperation::Until | FirRangeOperation::OpenEnd,
                ..
            }
        )
    }
}
