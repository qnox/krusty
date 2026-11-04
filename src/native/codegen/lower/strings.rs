//! What a program asks of a string beyond building one.
//!
//! Most of it is a table: `intrinsics::scalar_member` and `intrinsics::runtime_member` name the
//! runtime function for each member, because a string's members are all the runtime's. What lands
//! here is a property read, which arrives as a checked node rather than as a call, and exact
//! compiler intrinsics such as `String.get`, which must not be rediscovered from spellings.
//!
//! Written in source, `String.length` is not one of them: the frontend names that as a
//! compiler-supplied operation, so it never reaches a dependency-member path. Two shapes do reach
//! here — a receiver typed `CharSequence`, because the property belongs to the interface, and a
//! read this backend SYNTHESIZED for a reference to the property, which is an accessor call like
//! any other.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// An authenticated `CharSequence.trim` extension. The runtime slices contiguous text;
    /// arbitrary program-defined CharSequence implementations need their own virtual walk and
    /// must decline rather than reaching that reader. A reference cast preserves the operand's
    /// storage even when its checked result is the interface; the cast still executes normally.
    pub(super) fn text_trim(
        &mut self,
        receiver: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if ret != Ty::obj("kotlin/CharSequence") {
            return Err("an exact text trim with a mismatched result".into());
        }
        if !contiguous_trim_operand(self.file.ir, receiver) {
            crate::trace_compiler!(
                "native",
                "text trim receiver {receiver}: {:?}, checked {:?}",
                self.file.ir.expr(receiver),
                self.file.ir.checked_type(receiver)
            );
            return Err("an exact text trim without a contiguous text receiver".into());
        }
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_trim", &[any()], ret, &[value])
    }

    /// The selected `String.plus(Any?): String` declaration.
    pub(super) fn string_plus(
        &mut self,
        receiver: u32,
        argument: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let left = self.reference(receiver)?;
        let right = self.reference(argument)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_plus", &[any(), any()], ret, &[left, right])
    }

    /// A read of TEXT's `length` — `CharSequence`'s, a builder's, or a string's.
    ///
    /// One runtime function answers all three, because one runtime reader (`kt_text_of`) reaches
    /// the bytes of each, and reaches text the PROGRAM declared through the descriptor. That is the
    /// same position the member table already takes for `CharSequence.get`, and for the same
    /// reason.
    pub(super) fn is_text_length(&self, target: crate::fir::ExternalPropertyId) -> bool {
        let Some(property) = self.file.callables.property(target) else {
            return false;
        };
        let Some(getter) = self.file.callables.callable(property.getter) else {
            return false;
        };
        getter
            .compiler_intrinsic
            .or(getter.member_realization.compiler_intrinsic())
            == Some(crate::backend::BackendCompilerIntrinsic::StringLength)
            || super::super::super::intrinsics::is_non_string_text_length(
                getter.physical_owner,
                &property.name,
            )
    }

    pub(super) fn text_length(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_length", &[any()], Ty::Int, &[value])
    }

    /// The selected `String.get(Int): Char` declaration, carried by its compiler-intrinsic identity.
    pub(super) fn string_get(
        &mut self,
        receiver: u32,
        index: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        let Some(index) = self.coerce(index, Ty::Int)? else {
            return Err("a `Unit` string index".into());
        };
        if self.terminated {
            return Ok(None);
        }
        let produced = self.runtime_call(
            "kt_string_get",
            &[any(), Ty::Int],
            Ty::Char,
            &[value, index],
        )?;
        let Some(produced) = produced else {
            return Ok(None);
        };
        self.convert(produced, Some(Ty::Char), ret)
    }
}

fn contiguous_trim_text(ty: Ty) -> bool {
    ty == Ty::String
        || (!ty.is_nullable()
            && ty
                .obj_internal()
                .is_some_and(super::classifier_shapes::is_string_builder))
}

/// A checked reference cast never creates new storage. Look through only those operations to
/// prove the existing runtime's representation, without changing the evaluated receiver or
/// assuming that an arbitrary interface-typed parameter contains a String.
fn contiguous_trim_operand(ir: &IrFile, mut expression: u32) -> bool {
    for _ in 0..ir.exprs.len() {
        if ir
            .checked_type(expression)
            .is_some_and(contiguous_trim_text)
        {
            return true;
        }
        match ir.expr(expression) {
            IrExpr::TypeOp {
                op: IrTypeOp::Cast | IrTypeOp::CastNonNull,
                arg,
                type_operand,
            } if type_operand.is_reference() => expression = *arg,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_reference_casts_preserve_storage_but_unknown_receivers_do_not_prove_it() {
        let mut ir = IrFile::default();
        let text = ir.add_expr(IrExpr::Const(crate::ir::IrConst::String("  OK  ".into())));
        let cast = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::CastNonNull,
            arg: text,
            type_operand: Ty::obj("kotlin/CharSequence"),
        });
        assert!(contiguous_trim_operand(&ir, cast));
        let unknown = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(unknown, Ty::obj("kotlin/CharSequence"));
        let cast_unknown = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::CastNonNull,
            arg: unknown,
            type_operand: Ty::obj("kotlin/CharSequence"),
        });
        assert!(!contiguous_trim_operand(&ir, cast_unknown));
        let safe = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::SafeCast,
            arg: text,
            type_operand: Ty::obj("kotlin/CharSequence"),
        });
        assert!(!contiguous_trim_operand(&ir, safe));
    }

    #[test]
    fn trim_requires_a_proven_contiguous_runtime_text_representation() {
        assert!(contiguous_trim_text(Ty::String));
        assert!(contiguous_trim_text(Ty::obj("kotlin/text/StringBuilder")));
        for ty in [
            Ty::obj("kotlin/CharSequence"),
            Ty::obj("example/CharSequence"),
            Ty::obj("example/StringBuilder"),
            Ty::nullable(Ty::String),
            Ty::nullable(Ty::obj("kotlin/text/StringBuilder")),
            Ty::Int,
            Ty::Unit,
        ] {
            assert!(
                !contiguous_trim_text(ty),
                "unsupported representation {ty:?}"
            );
        }
    }
}
