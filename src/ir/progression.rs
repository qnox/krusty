use super::ExprId;
use crate::types::Ty;

/// A stdlib function a counted loop calls on its own, as resolution selected it. A backend calls
/// it as an ordinary external call of the selected declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct IrRuntimeFunction {
    pub function: crate::fir::ExternalCallableId,
    pub parameters: Vec<Ty>,
    pub result: Ty,
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
    /// A progression value read through the `first`, `last` and `step` members selected from its
    /// class (a `*Range` has no `step` read: it steps by 1). `setup` stores the value in a
    /// temporary when it is not a plain read; the member reads then read that temporary.
    ///
    /// `stepped` is the same loop as a `Step` over an `until` literal, which a target may count
    /// instead of building the progression (see `FirProgressionSource::Value`).
    Value {
        setup: Option<ExprId>,
        first: ExprId,
        last: ExprId,
        step: Option<ExprId>,
        stepped: Option<Box<IrProgressionSource>>,
    },
    /// `nested step step`, moving `last` with the selected `getProgressionLastElement`.
    Step {
        nested: Box<IrProgressionSource>,
        step: ExprId,
        last_element: IrRuntimeFunction,
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
                stepped,
            } => {
                setup.iter().for_each(|setup| f(*setup));
                f(*first);
                f(*last);
                step.iter().for_each(|step| f(*step));
                if let Some(stepped) = stepped {
                    stepped.visit(f);
                }
            }
            Self::Step { nested, step, .. } => {
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
                stepped,
            } => {
                if let Some(setup) = setup {
                    *setup = map(*setup);
                }
                *first = map(*first);
                *last = map(*last);
                if let Some(step) = step {
                    *step = map(*step);
                }
                if let Some(stepped) = stepped {
                    stepped.map_operands(map);
                }
            }
            Self::Step { nested, step, .. } => {
                nested.map_operands(map);
                *step = map(*step);
            }
            Self::Reversed(nested) => nested.map_operands(map),
        }
    }
}
