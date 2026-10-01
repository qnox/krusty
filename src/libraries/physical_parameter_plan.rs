//! Platform slots of one dependency callable, named by the provider that aligned the declaration
//! with its descriptor.

use crate::types::Ty;

/// One slot of [`crate::libraries::LibraryCallable::physical_params`].
///
/// Source ordinals are the declaration parameters a call supplies. A dispatch receiver or a suspend
/// continuation is an ABI slot the provider kept in that vector. Default-argument masks and the
/// marker are not slots of this vector; [`crate::libraries::DefaultCallRealization::mask_count`]
/// names that ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalParameterSlot {
    Source(u32),
    Dispatch,
    Continuation,
}

/// Name every slot of a classfile parameter vector from the roles established while its declaration
/// was aligned with metadata.
///
/// `dispatch` and `continuation` say that those exact ABI slots are present; this function validates
/// their vector shape but never infers either role from a length.
pub(crate) fn parameter_plan(
    source_len: usize,
    physical_len: usize,
    dispatch: bool,
    continuation: bool,
) -> Result<Box<[PhysicalParameterSlot]>, String> {
    let named = usize::from(dispatch) + source_len + usize::from(continuation);
    if physical_len != named {
        return Err(format!(
            "parameter alignment has {physical_len} physical slots for {source_len} source parameters (dispatch={dispatch}, continuation={continuation})"
        ));
    }
    let mut slots = Vec::with_capacity(physical_len);
    if dispatch {
        slots.push(PhysicalParameterSlot::Dispatch);
    }
    let source_len = u32::try_from(source_len).expect("too many source parameters");
    slots.extend((0..source_len).map(PhysicalParameterSlot::Source));
    if continuation {
        slots.push(PhysicalParameterSlot::Continuation);
    }
    Ok(slots.into_boxed_slice())
}

/// A vector whose every slot is a source parameter, in order.
pub(crate) fn source_parameter_plan(count: usize) -> Box<[PhysicalParameterSlot]> {
    parameter_plan(count, count, false, false).expect("a source vector aligns to source slots")
}

/// Physical types of the source parameters, in source order. ABI slots are not source arguments.
///
/// `plan` is required. A missing plan is not a source-parameter vector.
pub(crate) fn source_physical_parameters(
    physical: &[Ty],
    plan: Option<&[PhysicalParameterSlot]>,
) -> Result<Vec<Ty>, String> {
    let Some(plan) = plan else {
        return Err("a dependency callable has no physical parameter plan".to_string());
    };
    if plan.len() != physical.len() {
        return Err(format!(
            "physical parameter plan has {} slots for {} parameters",
            plan.len(),
            physical.len()
        ));
    }
    let mut source = Vec::new();
    for (ty, slot) in physical.iter().copied().zip(plan.iter().copied()) {
        match slot {
            PhysicalParameterSlot::Source(ordinal) => {
                let next = u32::try_from(source.len()).unwrap_or(u32::MAX);
                if ordinal != next {
                    return Err(format!(
                        "source parameter {ordinal} is not the next source slot {next}"
                    ));
                }
                source.push(ty);
            }
            PhysicalParameterSlot::Dispatch | PhysicalParameterSlot::Continuation => {}
        }
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::{parameter_plan, source_physical_parameters};
    use crate::libraries::PhysicalParameterSlot;
    use crate::types::Ty;

    #[test]
    fn alignment_names_source_dispatch_and_continuation_slots() {
        let source = parameter_plan(2, 2, false, false).unwrap();
        assert_eq!(
            source.as_ref(),
            &[
                PhysicalParameterSlot::Source(0),
                PhysicalParameterSlot::Source(1)
            ]
        );
        let dispatch = parameter_plan(1, 2, true, false).unwrap();
        assert_eq!(
            dispatch.as_ref(),
            &[
                PhysicalParameterSlot::Dispatch,
                PhysicalParameterSlot::Source(0)
            ]
        );
        let continuation = parameter_plan(1, 2, false, true).unwrap();
        assert_eq!(
            continuation.as_ref(),
            &[
                PhysicalParameterSlot::Source(0),
                PhysicalParameterSlot::Continuation
            ]
        );
        let both = parameter_plan(1, 3, true, true).unwrap();
        assert_eq!(
            both.as_ref(),
            &[
                PhysicalParameterSlot::Dispatch,
                PhysicalParameterSlot::Source(0),
                PhysicalParameterSlot::Continuation
            ]
        );
        assert!(parameter_plan(1, 3, false, false).is_err());
        assert!(parameter_plan(1, 0, false, false).is_err());
    }

    #[test]
    fn a_missing_plan_is_not_a_source_parameter_vector() {
        assert_eq!(
            source_physical_parameters(&[Ty::Int], None),
            Err("a dependency callable has no physical parameter plan".to_string())
        );
    }
}
