//! Consumption of the parameter mapping already decided by checked FIR.

use crate::ir::{ExprId, IrCheckedArgument};
use crate::types::Ty;

pub(super) enum CheckedArgumentValue {
    Expression(ExprId),
    VarargElement {
        value: ExprId,
        array_type: Ty,
        spread: bool,
    },
}

pub(super) enum CheckedArgumentSlot {
    Missing,
    Expression(ExprId),
    Default(u32),
    Vararg {
        array_type: Ty,
        elements: Vec<ExprId>,
        spreads: Vec<bool>,
    },
}

/// Lay checked arguments into semantic parameter slots while visiting values in source order.
///
/// The checker owns argument-to-parameter selection. Lowering only validates that the published
/// mapping is internally consistent, groups repeated vararg fragments, and gives the caller one
/// place to materialize each source value. `parameter_slot` accounts for non-value operands (for
/// example a local extension receiver) without creating a second mapping algorithm.
pub(super) fn materialize_checked_arguments(
    arguments: &[IrCheckedArgument],
    slot_count: usize,
    mut parameter_slot: impl FnMut(u32) -> Option<usize>,
    mut materialize: impl FnMut(usize, CheckedArgumentValue) -> Option<ExprId>,
) -> Option<Vec<CheckedArgumentSlot>> {
    let mut slots = (0..slot_count)
        .map(|_| CheckedArgumentSlot::Missing)
        .collect::<Vec<_>>();
    for argument in arguments {
        let parameter = match argument {
            IrCheckedArgument::Expression { parameter, .. }
            | IrCheckedArgument::Default { parameter }
            | IrCheckedArgument::Vararg { parameter, .. } => *parameter,
        };
        let slot = parameter_slot(parameter)?;
        let current = slots.get_mut(slot)?;
        match argument {
            IrCheckedArgument::Expression { value, .. } => {
                if !matches!(current, CheckedArgumentSlot::Missing) {
                    return None;
                }
                *current = CheckedArgumentSlot::Expression(materialize(
                    parameter as usize,
                    CheckedArgumentValue::Expression(*value),
                )?);
            }
            IrCheckedArgument::Default { .. } => {
                if !matches!(current, CheckedArgumentSlot::Missing) {
                    return None;
                }
                *current = CheckedArgumentSlot::Default(parameter);
            }
            IrCheckedArgument::Vararg {
                array_type,
                elements,
                ..
            } => {
                if matches!(current, CheckedArgumentSlot::Missing) {
                    *current = CheckedArgumentSlot::Vararg {
                        array_type: *array_type,
                        elements: Vec::new(),
                        spreads: Vec::new(),
                    };
                }
                let CheckedArgumentSlot::Vararg {
                    array_type: grouped_type,
                    elements: grouped_elements,
                    spreads,
                } = current
                else {
                    return None;
                };
                if *grouped_type != *array_type {
                    return None;
                }
                for (value, spread) in elements {
                    grouped_elements.push(materialize(
                        parameter as usize,
                        CheckedArgumentValue::VarargElement {
                            value: *value,
                            array_type: *array_type,
                            spread: *spread,
                        },
                    )?);
                    spreads.push(*spread);
                }
            }
        }
    }
    Some(slots)
}
