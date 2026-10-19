//! Calls of target-stdlib declarations whose implementation is one Wasm scalar instruction.
//!
//! Checked FIR and common IR keep such a call as an ordinary call of the selected dependency
//! declaration. Emission alone replaces it: the frozen callable fact carries the provider's
//! authenticated instruction or bit reinterpretation, and this module emits the operands and the
//! opcode of their physical class (and, for a conversion, the result's) instead of a call.

use super::*;
use crate::libraries::{CompilerIntrinsic, ScalarClass, ScalarComparison, ScalarInstruction};

impl Body<'_> {
    /// The scalar operation the provider authenticated as `target`'s implementation.
    pub(super) fn scalar_operation_of(
        &self,
        target: crate::fir::ExternalCallableId,
    ) -> Option<CompilerIntrinsic> {
        self.callables
            .callable(target)?
            .compiler_intrinsic
            .filter(|operation| is_scalar_operation(*operation))
    }

    /// Emit a call of `target`, a dependency declaration realized as one scalar instruction. An
    /// extension's receiver is the instruction's first operand, followed by the call's arguments.
    pub(super) fn scalar_operation_call(
        &mut self,
        target: crate::fir::ExternalCallableId,
        receiver: Option<u32>,
        params: &[Ty],
        ret: Ty,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let operation = self
            .scalar_operation_of(target)
            .ok_or_else(|| "a call of an unauthenticated scalar operation".to_string())?;
        let receiver_ty = match receiver {
            Some(_) => Some(
                self.callables
                    .callable(target)
                    .and_then(|callable| callable.source_receiver)
                    .ok_or_else(|| {
                        "a scalar operation call whose receiver the declaration lacks".to_string()
                    })?,
            ),
            None => None,
        };
        let params = receiver_ty
            .into_iter()
            .chain(params.iter().copied())
            .collect::<Vec<_>>();
        let operands = receiver
            .into_iter()
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        let malformed = || {
            format!(
                "a call of the scalar operation {operation:?} with parameters {} and result {}",
                params
                    .iter()
                    .map(|ty| describe(*ty))
                    .collect::<Vec<_>>()
                    .join(", "),
                describe(ret)
            )
        };
        let Some(&operand) = params.first() else {
            return Err(malformed());
        };
        let arity = match operation {
            CompilerIntrinsic::ScalarInstruction(instruction) => instruction.arity(),
            _ => 1,
        };
        if params.len() != arity
            || operands.len() != params.len()
            || params.iter().any(|ty| *ty != operand)
        {
            return Err(malformed());
        }
        let opcode = match operation {
            CompilerIntrinsic::ScalarInstruction(instruction) => opcode(instruction, operand, ret),
            _ => reinterpret_opcode(operand, ret).map(Opcode::Plain),
        }
        .ok_or_else(malformed)?;
        for value in operands {
            self.value(value, operand)?;
        }
        match opcode {
            Opcode::Plain(opcode) => self.code.op(opcode),
            Opcode::Saturating(opcode) => self.code.trunc_sat(opcode),
        };
        Ok(())
    }
}

/// Whether the Wasm emitter realizes `operation` as one scalar instruction at its call.
pub(in crate::wasm::codegen) fn is_scalar_operation(operation: CompilerIntrinsic) -> bool {
    matches!(
        operation,
        CompilerIntrinsic::ScalarInstruction(_) | CompilerIntrinsic::ReinterpretBits
    )
}

/// The `reinterpret` opcode between equal-width integral and floating scalars.
fn reinterpret_opcode(from: Ty, to: Ty) -> Option<u8> {
    Some(match (from, to) {
        (Ty::Float, Ty::Int) => 0xbc,
        (Ty::Double, Ty::Long) => 0xbd,
        (Ty::Int, Ty::Float) => 0xbe,
        (Ty::Long, Ty::Double) => 0xbf,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opcode {
    Plain(u8),
    /// A saturating truncation behind the `0xfc` prefix.
    Saturating(u8),
}

/// The opcode of `instruction` over `operand` operands producing `result`, if the instruction
/// admits those types.
fn opcode(instruction: ScalarInstruction, operand: Ty, result: Ty) -> Option<Opcode> {
    use ScalarClass::{Float32, Float64, Integral32, Integral64};
    use ScalarInstruction::*;
    if !instruction.admits(operand, result) {
        return None;
    }
    let from = ScalarClass::of(operand)?;
    let to = ScalarClass::of(result);
    // Each class's block in the opcode table: i32 arithmetic starts at `i32.clz`, i64 at
    // `i64.clz`, f32 at `f32.abs` and f64 at `f64.abs`.
    let integral = |base: u8| -> u8 {
        let offset = match instruction {
            CountLeadingZeros => 0,
            CountTrailingZeros => 1,
            PopulationCount => 2,
            Add => 3,
            Subtract => 4,
            Multiply => 5,
            Divide => 6,
            DivideUnsigned => 7,
            Remainder => 8,
            RemainderUnsigned => 9,
            And => 10,
            Or => 11,
            Xor => 12,
            ShiftLeft => 13,
            ShiftRight => 14,
            ShiftRightUnsigned => 15,
            RotateLeft => 16,
            _ => 17,
        };
        base + offset
    };
    let floating = |base: u8| -> u8 {
        let offset = match instruction {
            Absolute => 0,
            Negate => 1,
            Ceiling => 2,
            Floor => 3,
            Truncate => 4,
            Nearest => 5,
            SquareRoot => 6,
            Add => 7,
            Subtract => 8,
            Multiply => 9,
            Divide => 10,
            Minimum => 11,
            Maximum => 12,
            _ => 13,
        };
        base + offset
    };
    Some(Opcode::Plain(match instruction {
        Compare(comparison) => compare(comparison, from),
        Wrap => 0xa7,
        Extend => 0xac,
        ExtendUnsigned => 0xad,
        ConvertToFloating | ConvertUnsignedToFloating => {
            let unsigned = u8::from(instruction == ConvertUnsignedToFloating);
            let base = match to? {
                Float32 => 0xb2,
                Float64 => 0xb7,
                Integral32 | Integral64 => return None,
            };
            base + 2 * u8::from(from == Integral64) + unsigned
        }
        TruncateSaturating | TruncateSaturatingUnsigned => {
            let unsigned = u8::from(instruction == TruncateSaturatingUnsigned);
            let base = match to? {
                Integral32 => 0,
                Integral64 => 4,
                Float32 | Float64 => return None,
            };
            return Some(Opcode::Saturating(
                base + 2 * u8::from(from == Float64) + unsigned,
            ));
        }
        Demote => 0xb6,
        Promote => 0xbb,
        _ => match from {
            Integral32 => integral(0x67),
            Integral64 => integral(0x79),
            Float32 => floating(0x8b),
            Float64 => floating(0x99),
        },
    }))
}

fn compare(comparison: ScalarComparison, class: ScalarClass) -> u8 {
    use ScalarComparison::*;
    match class {
        ScalarClass::Integral32 | ScalarClass::Integral64 => {
            let base = if class == ScalarClass::Integral32 {
                0x46
            } else {
                0x51
            };
            base + match comparison {
                Equal => 0,
                NotEqual => 1,
                Less => 2,
                LessUnsigned => 3,
                Greater => 4,
                GreaterUnsigned => 5,
                LessOrEqual => 6,
                LessOrEqualUnsigned => 7,
                GreaterOrEqual => 8,
                GreaterOrEqualUnsigned => 9,
            }
        }
        ScalarClass::Float32 | ScalarClass::Float64 => {
            let base = if class == ScalarClass::Float32 {
                0x5b
            } else {
                0x61
            };
            // `admits` rejects an unsigned comparison of floating operands.
            base + match comparison {
                Equal => 0,
                NotEqual => 1,
                Less | LessUnsigned => 2,
                Greater | GreaterUnsigned => 3,
                LessOrEqual | LessOrEqualUnsigned => 4,
                GreaterOrEqual | GreaterOrEqualUnsigned => 5,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{opcode, Opcode};
    use crate::libraries::{ScalarComparison, ScalarInstruction};
    use crate::types::Ty;

    #[test]
    fn opcodes_match_the_wasm_numeric_instruction_table() {
        let plain = |instruction, operand, result| opcode(instruction, operand, result);
        assert_eq!(
            plain(ScalarInstruction::CountLeadingZeros, Ty::Int, Ty::Int),
            Some(Opcode::Plain(0x67))
        );
        assert_eq!(
            plain(ScalarInstruction::RotateRight, Ty::Int, Ty::Int),
            Some(Opcode::Plain(0x78))
        );
        assert_eq!(
            plain(ScalarInstruction::RotateRight, Ty::Long, Ty::Long),
            Some(Opcode::Plain(0x8a))
        );
        assert_eq!(
            plain(ScalarInstruction::DivideUnsigned, Ty::ULong, Ty::ULong),
            Some(Opcode::Plain(0x80))
        );
        assert_eq!(
            plain(ScalarInstruction::CopySign, Ty::Float, Ty::Float),
            Some(Opcode::Plain(0x98))
        );
        assert_eq!(
            plain(ScalarInstruction::CopySign, Ty::Double, Ty::Double),
            Some(Opcode::Plain(0xa6))
        );
        assert_eq!(
            plain(ScalarInstruction::Divide, Ty::Double, Ty::Double),
            Some(Opcode::Plain(0xa3))
        );
        assert_eq!(
            plain(
                ScalarInstruction::Compare(ScalarComparison::GreaterOrEqualUnsigned),
                Ty::Int,
                Ty::Boolean
            ),
            Some(Opcode::Plain(0x4f))
        );
        assert_eq!(
            plain(
                ScalarInstruction::Compare(ScalarComparison::GreaterOrEqualUnsigned),
                Ty::Long,
                Ty::Boolean
            ),
            Some(Opcode::Plain(0x5a))
        );
        assert_eq!(
            plain(
                ScalarInstruction::Compare(ScalarComparison::GreaterOrEqual),
                Ty::Double,
                Ty::Boolean
            ),
            Some(Opcode::Plain(0x66))
        );
        assert_eq!(
            plain(ScalarInstruction::Wrap, Ty::Long, Ty::Int),
            Some(Opcode::Plain(0xa7))
        );
        assert_eq!(
            plain(ScalarInstruction::ExtendUnsigned, Ty::Int, Ty::ULong),
            Some(Opcode::Plain(0xad))
        );
        assert_eq!(
            plain(
                ScalarInstruction::ConvertUnsignedToFloating,
                Ty::Long,
                Ty::Float
            ),
            Some(Opcode::Plain(0xb5))
        );
        assert_eq!(
            plain(ScalarInstruction::ConvertToFloating, Ty::Int, Ty::Double),
            Some(Opcode::Plain(0xb7))
        );
        assert_eq!(
            plain(
                ScalarInstruction::ConvertUnsignedToFloating,
                Ty::Long,
                Ty::Double
            ),
            Some(Opcode::Plain(0xba))
        );
        assert_eq!(
            plain(ScalarInstruction::Demote, Ty::Double, Ty::Float),
            Some(Opcode::Plain(0xb6))
        );
        assert_eq!(
            plain(ScalarInstruction::Promote, Ty::Float, Ty::Double),
            Some(Opcode::Plain(0xbb))
        );
        assert_eq!(
            plain(ScalarInstruction::TruncateSaturating, Ty::Float, Ty::Int),
            Some(Opcode::Saturating(0))
        );
        assert_eq!(
            plain(
                ScalarInstruction::TruncateSaturatingUnsigned,
                Ty::Double,
                Ty::UInt
            ),
            Some(Opcode::Saturating(3))
        );
        assert_eq!(
            plain(
                ScalarInstruction::TruncateSaturatingUnsigned,
                Ty::Double,
                Ty::ULong
            ),
            Some(Opcode::Saturating(7))
        );
        assert_eq!(plain(ScalarInstruction::SquareRoot, Ty::Int, Ty::Int), None);
    }
}
