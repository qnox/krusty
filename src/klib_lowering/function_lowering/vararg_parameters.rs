//! A vararg declaration is an array parameter, not an expanded calling convention.

use super::{mismatch, KlibBodyDeclineReason, Ty, TyKind};
use crate::libraries::KlibBodyCallable;

pub(in super::super) fn array_element(packed: Ty) -> Result<Ty, KlibBodyDeclineReason> {
    if !packed.is_array()
        || matches!(packed.kind(), TyKind::Obj(_, arguments)
            if arguments.len() != usize::from(packed.is_reference_array()))
    {
        return Err(mismatch(
            "a vararg parameter is not a non-null packed array".into(),
        ));
    }
    match packed
        .array_elem()
        .expect("an authenticated array has an element")
        .kind()
    {
        TyKind::OutProjection(element) => Ok(element),
        TyKind::InProjection(_) | TyKind::StarProjection(_) => Err(mismatch(
            "a vararg packed array has an incompatible element projection".into(),
        )),
        _ => Ok(packed.array_elem().unwrap()),
    }
}

pub(super) fn reject_dispatch_marker(
    receiver: Option<&crate::metadata::klib_ir::tree::KlibIrParameter>,
) -> Result<(), KlibBodyDeclineReason> {
    if receiver.is_some_and(|receiver| receiver.vararg_element_type.is_some()) {
        return Err(mismatch(
            "a serialized dispatch receiver carries a vararg marker".into(),
        ));
    }
    Ok(())
}

#[derive(Default)]
pub(in super::super) struct VarargParameters {
    index: Option<usize>,
}

impl VarargParameters {
    pub(in super::super) fn record(
        &mut self,
        index: usize,
        regular_value: bool,
        packed: Ty,
        element: Ty,
    ) -> Result<(), KlibBodyDeclineReason> {
        if !regular_value || self.index.is_some() {
            return Err(mismatch(
                "a serialized vararg is not a unique value parameter".into(),
            ));
        }
        if array_element(packed)? != element {
            return Err(mismatch(
                "the serialized vararg element disagrees with its packed array".into(),
            ));
        }
        self.index = Some(index);
        Ok(())
    }

    pub(in super::super) fn authenticate(
        self,
        selected: Option<KlibBodyCallable<'_>>,
        companion_receiver: bool,
    ) -> Result<(), KlibBodyDeclineReason> {
        if let Some(selected) = selected {
            let expected = selected.vararg_index().map(|index| {
                index + usize::from(companion_receiver && index >= selected.context_count())
            });
            if self.index != expected {
                return Err(mismatch(
                    "the serialized vararg position disagrees with its selected declaration".into(),
                ));
            }
        }
        Ok(())
    }
}
