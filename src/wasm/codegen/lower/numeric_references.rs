//! Numeric carriers belong to authenticated generic dependency ABI slots.
//!
//! Ordinary expressions, source casts and concrete Any parameters keep their existing lowering.

use super::{carrier, Body, Ty, TyKind, Unsupported};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumericSlot {
    Int,
    Long,
}

impl NumericSlot {
    fn selected(declared: Ty, applied: Ty) -> Option<Self> {
        if !declared.is_ty_param() {
            return None;
        }
        match applied {
            Ty::Int => Some(Self::Int),
            Ty::Long => Some(Self::Long),
            _ => None,
        }
    }
}

/// A new numeric realization must cover every occurrence of that same declaration formal.
/// An unrelated nullable parameter does not restrict another formal's numeric ABI. A null-only
/// reference call introduces no numeric realization and retains its existing reference path.
pub(super) fn authenticate_numeric_formals(
    parameters: &[Ty],
    applied_parameters: &[Ty],
    result: Ty,
    applied_result: Ty,
) -> Result<(), Unsupported> {
    if parameters.len() != applied_parameters.len() {
        return Err("a numeric dependency ABI without matching checked slots".into());
    }
    let slots = || {
        parameters
            .iter()
            .copied()
            .zip(applied_parameters.iter().copied())
            .chain([(result, applied_result)])
    };
    for (declared, applied) in slots() {
        if NumericSlot::selected(declared, applied).is_none() {
            continue;
        }
        let TyKind::TyParam(formal, _) = declared.kind() else {
            unreachable!("numeric slots are declaration formals");
        };
        let names = [formal.to_string()];
        for (other, applied_other) in slots() {
            if crate::types::ty_mentions_param(other, &names) && carrier(applied_other).is_err() {
                return Err("a numeric instantiation of an unsupported generic ABI slot".into());
            }
        }
    }
    Ok(())
}

impl Body<'_> {
    pub(super) fn dependency_value(
        &mut self,
        argument: u32,
        declared: Ty,
        applied: Ty,
    ) -> Result<(), Unsupported> {
        let Some(slot) = NumericSlot::selected(declared, applied) else {
            return self.value(argument, declared);
        };
        // Evaluate the checked operand normally. Numeric permission never enters its subtree.
        self.value(argument, applied)?;
        self.code.call(match slot {
            NumericSlot::Int => self.runtime.numbers.int.box_value,
            NumericSlot::Long => self.runtime.numbers.long.box_value,
        });
        Ok(())
    }

    pub(super) fn dependency_result(
        &mut self,
        declared: Ty,
        applied: Ty,
    ) -> Result<(), Unsupported> {
        let Some(slot) = NumericSlot::selected(declared, applied) else {
            return self.coerce_emitted_value(declared, applied);
        };
        self.code.call(match slot {
            NumericSlot::Int => self.runtime.numbers.int.unbox_value,
            NumericSlot::Long => self.runtime.numbers.long.unbox_value,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_permission_requires_a_declared_formal_and_exact_nonnullable_application() {
        let formal = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert_eq!(
            NumericSlot::selected(formal, Ty::Int),
            Some(NumericSlot::Int)
        );
        assert_eq!(
            NumericSlot::selected(formal, Ty::Long),
            Some(NumericSlot::Long)
        );
        for applied in [
            Ty::nullable(Ty::Int),
            Ty::Float,
            Ty::Double,
            Ty::UInt,
            Ty::String,
        ] {
            assert_eq!(NumericSlot::selected(formal, applied), None);
        }
        assert_eq!(NumericSlot::selected(Ty::obj("kotlin/Any"), Ty::Int), None);
        assert_eq!(NumericSlot::selected(Ty::nullable(formal), Ty::Int), None);
    }

    #[test]
    fn nullable_occurrences_restrict_only_the_realized_formal() {
        let formal = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert_eq!(
            authenticate_numeric_formals(
                &[formal, Ty::nullable(Ty::Int)],
                &[Ty::Int, Ty::nullable(Ty::Int)],
                formal,
                Ty::Int,
            ),
            Ok(()),
        );
        assert_eq!(
            authenticate_numeric_formals(
                &[Ty::nullable(formal)],
                &[Ty::nullable(Ty::Int)],
                formal,
                Ty::Int,
            ),
            Err("a numeric instantiation of an unsupported generic ABI slot".into()),
        );
    }
}
