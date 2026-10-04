//! kotlinc's `CharSequenceIterationHandler`: an indexed loop over a `CharSequence` other than a
//! `String` reads the selected `CharSequence.length` before every iteration, since the sequence may
//! change length while the loop runs, and reads each element with the selected
//! `CharSequence.get`.

use crate::fir::FirCharSequenceIndexing;
use crate::ir::{ExprId, IrBinOp, IrCheckedArgument, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    /// The condition `index < receiver.length` and the element `receiver.get(index)` of an indexed
    /// loop over the `CharSequence` stored in `receiver` (of type `receiver_ty`) with its counter
    /// in `index`.
    pub(super) fn char_sequence_indexing(
        &mut self,
        indexing: &FirCharSequenceIndexing,
        receiver: u32,
        receiver_ty: Ty,
        index: u32,
    ) -> Result<(ExprId, ExprId), FirLoweringFailure> {
        let length_receiver = self.char_sequence_receiver(indexing, receiver, receiver_ty)?;
        let length = self.member_property_read(&indexing.length, length_receiver)?;
        let index_read = self.ir.add_expr(IrExpr::GetValue(index));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: index_read,
            rhs: length,
        });
        let get_receiver = self.char_sequence_receiver(indexing, receiver, receiver_ty)?;
        let index_read = self.ir.add_expr(IrExpr::GetValue(index));
        let element = self.loop_call(
            &indexing.get,
            Some(get_receiver),
            None,
            &[IrCheckedArgument::Expression {
                parameter: 0,
                value: index_read,
            }],
            &[],
        )?;
        Ok((condition, element))
    }

    /// A read of the stored receiver as the `CharSequence` the selected members are declared on:
    /// an implicit cast when it is typed as anything else.
    fn char_sequence_receiver(
        &mut self,
        indexing: &FirCharSequenceIndexing,
        receiver: u32,
        receiver_ty: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let crate::fir::FirPropertyTarget::External {
            receiver: Some(char_sequence),
            ..
        } = &indexing.length
        else {
            return Err(FirLoweringFailure::UnsupportedLoopMember);
        };
        let char_sequence = char_sequence.get();
        let read = self.ir.add_expr(IrExpr::GetValue(receiver));
        if receiver_ty == char_sequence {
            return Ok(read);
        }
        Ok(self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: read,
            type_operand: char_sequence,
        }))
    }
}
