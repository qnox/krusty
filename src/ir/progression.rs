use super::ExprId;

/// A member of a progression value that a counted loop reads (`first`, `last`, `step`). Common
/// lowering selects which one; the backend owns the member's physical accessor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrProgressionMember {
    First,
    Last,
    Step,
}

/// The progression a checked counted loop iterates, as the checker matched it (kotlinc's
/// `HeaderInfoBuilder`). Operands are lowered expressions; each backend builds its loop header and
/// control flow from this tree at its own boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum IrProgressionSource {
    /// `start..end`, `start..<end`, `start downTo end` or `start until end`.
    Literal {
        operation: crate::fir::FirRangeOperation,
        start: ExprId,
        end: ExprId,
    },
    /// A progression value read through its `first`, `last` and `step`. `iterable` already has
    /// the progression's static class.
    Value {
        progression: crate::fir::FirProgressionClass,
        iterable: ExprId,
    },
    /// `nested step step`.
    Step {
        nested: Box<IrProgressionSource>,
        step: ExprId,
    },
    /// `nested.reversed()`.
    Reversed(Box<IrProgressionSource>),
}

impl IrProgressionSource {
    /// Every operand, in evaluation order.
    pub fn operands(&self) -> Vec<ExprId> {
        let mut operands = Vec::new();
        self.visit(&mut |operand| operands.push(operand));
        operands
    }

    fn visit(&self, f: &mut impl FnMut(ExprId)) {
        match self {
            Self::Literal { start, end, .. } => {
                f(*start);
                f(*end);
            }
            Self::Value { iterable, .. } => f(*iterable),
            Self::Step { nested, step } => {
                nested.visit(f);
                f(*step);
            }
            Self::Reversed(nested) => nested.visit(f),
        }
    }

    /// Replaces every operand with `map(operand)`.
    pub fn map_operands(&mut self, map: &mut impl FnMut(ExprId) -> ExprId) {
        match self {
            Self::Literal { start, end, .. } => {
                *start = map(*start);
                *end = map(*end);
            }
            Self::Value { iterable, .. } => *iterable = map(*iterable),
            Self::Step { nested, step } => {
                nested.map_operands(map);
                *step = map(*step);
            }
            Self::Reversed(nested) => nested.map_operands(map),
        }
    }
}
