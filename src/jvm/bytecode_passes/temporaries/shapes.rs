//! The instruction shapes the temporaries rules match.

use crate::jvm::method_node::{Constant, Insn};

pub(super) const NOP: u8 = 0x00;
pub(super) const POP: u8 = 0x57;
pub(super) const POP2: u8 = 0x58;
pub(super) const DUP: u8 = 0x59;
pub(super) const SWAP: u8 = 0x5f;
pub(super) const IFNULL: u8 = 0xc6;
pub(super) const IFNONNULL: u8 = 0xc7;
const ILOAD: u8 = 0x15;
const ALOAD: u8 = 0x19;
const ISTORE: u8 = 0x36;
const ASTORE: u8 = 0x3a;
const GOTO: u8 = 0xa7;
const IRETURN: u8 = 0xac;
const RETURN: u8 = 0xb1;
const ATHROW: u8 = 0xbf;
const GETSTATIC: u8 = 0xb2;
const INVOKESTATIC: u8 = 0xb8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Int,
    Long,
    Float,
    Double,
    Reference,
}

impl Kind {
    fn of(offset: u8) -> Kind {
        match offset {
            0 => Kind::Int,
            1 => Kind::Long,
            2 => Kind::Float,
            3 => Kind::Double,
            _ => Kind::Reference,
        }
    }

    pub(super) fn words(self) -> u16 {
        match self {
            Kind::Long | Kind::Double => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VarOp {
    Load(Kind, u16),
    Store(Kind, u16),
    Iinc(u16),
}

pub(super) fn var_op(insn: &Insn) -> Option<VarOp> {
    match *insn {
        Insn::Var { op, slot } if (ILOAD..=ALOAD).contains(&op) => {
            Some(VarOp::Load(Kind::of(op - ILOAD), slot))
        }
        Insn::Var { op, slot } if (ISTORE..=ASTORE).contains(&op) => {
            Some(VarOp::Store(Kind::of(op - ISTORE), slot))
        }
        Insn::Iinc { slot, .. } => Some(VarOp::Iinc(slot)),
        _ => None,
    }
}

/// Whether `insn` loads a reference, from `slot` when one is given.
pub(super) fn loads_reference(insn: &Insn, slot: Option<u16>) -> bool {
    matches!(var_op(insn), Some(VarOp::Load(Kind::Reference, s)) if slot.is_none_or(|slot| slot == s))
}

pub(super) fn is_op(insn: &Insn, op: u8) -> bool {
    *insn == Insn::Op(op)
}

/// A `goto` or `athrow`: nothing falls through it into the next instruction. A return does, as
/// kotlinc writes it: `xreturn; nop`, the dead `nop` removed only by a later pass.
pub(super) fn ends_flow(insn: &Insn) -> bool {
    matches!(insn, Insn::Jump { op: GOTO, .. }) || is_op(insn, ATHROW)
}

/// A `goto`, a return or `athrow`: an instruction nothing after it is reached from.
pub(super) fn is_terminator(insn: &Insn) -> bool {
    matches!(insn, Insn::Jump { op: GOTO, .. })
        || matches!(insn, Insn::Op(op) if (IRETURN..=RETURN).contains(op) || *op == ATHROW)
}

/// A value a `swap` can put a kept value under: an `aload`, or a `getstatic` of a one-word field.
pub(super) fn is_swappable(insn: &Insn) -> bool {
    loads_reference(insn, None)
        || matches!(insn, Insn::Field { op: GETSTATIC, desc, .. } if desc != "J" && desc != "D")
}

pub(super) fn is_string_constant(insn: &Insn) -> bool {
    matches!(insn, Insn::Ldc(Constant::String(_)))
}

/// `Intrinsics.checkNotNullExpressionValue` or the older `checkExpressionValueIsNotNull`.
pub(super) fn is_expression_null_check(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Method { op: INVOKESTATIC, owner, name, desc, .. }
            if owner == "kotlin/jvm/internal/Intrinsics"
                && desc == "(Ljava/lang/Object;Ljava/lang/String;)V"
                && matches!(
                    name.as_str(),
                    "checkNotNullExpressionValue" | "checkExpressionValueIsNotNull"
                )
    )
}

/// The opcode and target of an `ifnull` or `ifnonnull`.
pub(super) fn null_jump(insn: &Insn) -> Option<(u8, crate::jvm::method_node::LabelId)> {
    match *insn {
        Insn::Jump {
            op: op @ (IFNULL | IFNONNULL),
            target,
        } => Some((op, target)),
        _ => None,
    }
}
