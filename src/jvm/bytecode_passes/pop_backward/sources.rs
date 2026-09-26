//! The analysis `PopBackwardPropagationTransformer` runs: ASM's `SourceInterpreter` (every value
//! is the set of instructions that can have pushed it), as kotlinc's `HazardsTrackingInterpreter`
//! extends it. Every value an instruction consumes other than by `pop`/`pop2`/`return` marks the
//! instructions that pushed it as ones the pass must not touch: a load, store, `dup`, `swap`,
//! arithmetic, jump, field or array access, call or `throw` needs the value to exist.

use std::collections::BTreeSet;

use super::super::analysis::{opcode, AnalyzerError, At, Interpreter, Value};
use super::super::descriptors;
use super::super::opcodes::*;
use crate::jvm::method_node::{Constant, Insn};

const LADD: u8 = 0x61;
const DADD: u8 = 0x63;
const LSUB: u8 = 0x65;
const DSUB: u8 = 0x67;
const LMUL: u8 = 0x69;
const DMUL: u8 = 0x6b;
const LDIV: u8 = 0x6d;
const DDIV: u8 = 0x6f;
const LREM: u8 = 0x71;
const LSHL: u8 = 0x79;
const LSHR: u8 = 0x7b;
const LUSHR: u8 = 0x7d;
const LAND: u8 = 0x7f;
const LOR: u8 = 0x81;

/// ASM's `SourceValue`: the words the value takes and the node indices of the instructions that
/// can have pushed it (none for a parameter, an unassigned slot or a caught exception).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceValue {
    pub size: usize,
    pub insns: BTreeSet<usize>,
}

impl Value for SourceValue {
    fn size(&self) -> usize {
        self.size
    }
}

impl SourceValue {
    fn pushed(size: usize, at: &At) -> SourceValue {
        SourceValue {
            size,
            insns: BTreeSet::from([at.index]),
        }
    }
}

/// `HazardsTrackingInterpreter`: [`SourceValue`]s, and the instructions whose value some
/// instruction other than a `pop` consumes (`dontTouchInsnIndices`).
pub(super) struct HazardsTracking {
    pub dont_touch: Vec<bool>,
}

impl HazardsTracking {
    pub fn new(nodes: usize) -> HazardsTracking {
        HazardsTracking {
            dont_touch: vec![false; nodes],
        }
    }

    fn mark(&mut self, value: &SourceValue) {
        for &insn in &value.insns {
            self.dont_touch[insn] = true;
        }
    }
}

/// The words a field or method descriptor's value takes (`0` for `void`).
fn words(descriptor: &str) -> usize {
    if descriptor == "V" {
        0
    } else {
        descriptors::size(descriptor)
    }
}

fn field_words(insn: &Insn) -> usize {
    match insn {
        Insn::Field { desc, .. } => words(desc),
        _ => 1,
    }
}

impl Interpreter for HazardsTracking {
    type V = SourceValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<SourceValue> {
        let size = match ty {
            Some("V") => return None,
            Some(ty) => words(ty),
            None => 1,
        };
        Some(SourceValue {
            size,
            insns: BTreeSet::new(),
        })
    }

    fn new_operation(&mut self, at: &At) -> Result<SourceValue, AnalyzerError> {
        let size = match at.insn {
            Insn::Op(LCONST_0 | LCONST_1 | DCONST_0 | DCONST_1)
            | Insn::Ldc(Constant::Long(_) | Constant::Double(_)) => 2,
            Insn::Field { op: GETSTATIC, .. } => field_words(at.insn),
            _ => 1,
        };
        Ok(SourceValue::pushed(size, at))
    }

    fn copy_operation(
        &mut self,
        at: &At,
        value: &SourceValue,
    ) -> Result<SourceValue, AnalyzerError> {
        self.mark(value);
        Ok(SourceValue::pushed(value.size, at))
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &SourceValue,
    ) -> Result<Option<SourceValue>, AnalyzerError> {
        self.mark(value);
        let size = match opcode(at.insn) {
            LNEG | DNEG | I2L | I2D | L2D | F2L | F2D | D2L => 2,
            GETFIELD => field_words(at.insn),
            _ => 1,
        };
        Ok(Some(SourceValue::pushed(size, at)))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &SourceValue,
        second: &SourceValue,
    ) -> Result<Option<SourceValue>, AnalyzerError> {
        self.mark(first);
        self.mark(second);
        let size = match opcode(at.insn) {
            LALOAD | DALOAD | LADD | DADD | LSUB | DSUB | LMUL | DMUL | LDIV | DDIV | LREM
            | DREM | LSHL | LSHR | LUSHR | LAND | LOR | LXOR => 2,
            _ => 1,
        };
        Ok(Some(SourceValue::pushed(size, at)))
    }

    fn ternary_operation(
        &mut self,
        at: &At,
        first: &SourceValue,
        second: &SourceValue,
        third: &SourceValue,
    ) -> Result<Option<SourceValue>, AnalyzerError> {
        self.mark(first);
        self.mark(second);
        self.mark(third);
        Ok(Some(SourceValue::pushed(1, at)))
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[SourceValue],
    ) -> Result<Option<SourceValue>, AnalyzerError> {
        for value in values {
            self.mark(value);
        }
        let size = match at.insn {
            Insn::Method { desc, .. } | Insn::InvokeDynamic { desc, .. } => {
                descriptors::return_type(desc).map_or(1, words)
            }
            _ => 1,
        };
        Ok(Some(SourceValue::pushed(size, at)))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &SourceValue,
        _expected: &SourceValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    /// ASM's `SourceInterpreter.merge`: the union of the sources, at the smaller size.
    fn merge(&mut self, first: &SourceValue, second: &SourceValue) -> SourceValue {
        if first.size == second.size && second.insns.is_subset(&first.insns) {
            return first.clone();
        }
        SourceValue {
            size: first.size.min(second.size),
            insns: first.insns.union(&second.insns).copied().collect(),
        }
    }
}
