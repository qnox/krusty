use super::ExprId;

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
    /// A progression value read through the `first`, `last` and `step` members selected from its
    /// class (a `*Range` has no `step` read: it steps by 1). `setup` stores the value in a
    /// temporary when it is not a plain read; the member reads then read that temporary.
    Value {
        setup: Option<ExprId>,
        first: ExprId,
        last: ExprId,
        step: Option<ExprId>,
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
            Self::Value {
                setup,
                first,
                last,
                step,
            } => {
                setup.iter().for_each(|setup| f(*setup));
                f(*first);
                f(*last);
                step.iter().for_each(|step| f(*step));
            }
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
            Self::Value {
                setup,
                first,
                last,
                step,
            } => {
                if let Some(setup) = setup {
                    *setup = map(*setup);
                }
                *first = map(*first);
                *last = map(*last);
                if let Some(step) = step {
                    *step = map(*step);
                }
            }
            Self::Step { nested, step } => {
                nested.map_operands(map);
                *step = map(*step);
            }
            Self::Reversed(nested) => nested.map_operands(map),
        }
    }
}
