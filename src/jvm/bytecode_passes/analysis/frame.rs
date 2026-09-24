//! ASM's `Frame` and `Interpreter` (`org.objectweb.asm.tree.analysis`): the abstract state before
//! one instruction, and how each instruction transforms it. kotlinc's analyses are interpreters over
//! this frame, so the execution rules here are ASM's, instruction for instruction.

use super::super::descriptors;
use super::super::opcodes::*;
use super::fast_analyzer::AnalyzerError;
use super::opcode;
use crate::jvm::method_node::Insn;

/// An abstract value. `size` is the words it occupies (2 for `long`/`double`).
pub(crate) trait Value: Clone + PartialEq {
    fn size(&self) -> usize;
}

/// The instruction an interpreter is asked about: its node index and the instruction.
pub(crate) struct At<'a> {
    pub index: usize,
    pub insn: &'a Insn,
}

/// ASM's `Interpreter`: what each kind of instruction makes of its operands. `Ok(None)` is ASM's
/// `null` result (an instruction that produces nothing).
pub(crate) trait Interpreter {
    type V: Value;

    /// A value of the field descriptor `ty`; `None` for `void`. `ty: None` is ASM's `newValue(null)`,
    /// an uninitialized slot.
    fn new_value(&mut self, ty: Option<&str>) -> Option<Self::V>;
    fn new_operation(&mut self, at: &At) -> Result<Self::V, AnalyzerError>;
    fn copy_operation(&mut self, at: &At, value: &Self::V) -> Result<Self::V, AnalyzerError>;
    fn unary_operation(
        &mut self,
        at: &At,
        value: &Self::V,
    ) -> Result<Option<Self::V>, AnalyzerError>;
    fn binary_operation(
        &mut self,
        at: &At,
        first: &Self::V,
        second: &Self::V,
    ) -> Result<Option<Self::V>, AnalyzerError>;
    fn ternary_operation(
        &mut self,
        at: &At,
        first: &Self::V,
        second: &Self::V,
        third: &Self::V,
    ) -> Result<Option<Self::V>, AnalyzerError>;
    fn nary_operation(
        &mut self,
        at: &At,
        values: &[Self::V],
    ) -> Result<Option<Self::V>, AnalyzerError>;
    fn return_operation(
        &mut self,
        at: &At,
        value: &Self::V,
        expected: &Self::V,
    ) -> Result<(), AnalyzerError>;
    fn merge(&mut self, first: &Self::V, second: &Self::V) -> Self::V;

    fn new_empty_value(&mut self) -> Self::V {
        self.new_value(None)
            .expect("an interpreter's uninitialized value exists")
    }
    fn new_parameter_value(&mut self, ty: &str) -> Self::V {
        self.new_value(Some(ty))
            .expect("a parameter's type is not void")
    }
    fn new_exception_value(&mut self, catch_internal_name: &str) -> Self::V {
        self.new_value(Some(&descriptors::of_internal_name(catch_internal_name)))
            .expect("an exception type is a reference")
    }
}

/// The state before one instruction: locals by slot, the operand stack bottom-first, and the
/// method's return value (`None` for `void`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Frame<V> {
    pub locals: Vec<V>,
    pub stack: Vec<V>,
    pub return_value: Option<V>,
}

fn var_slot(at: &At) -> Result<usize, AnalyzerError> {
    match at.insn {
        Insn::Var { slot, .. } => Ok(usize::from(*slot)),
        _ => Err(error(at, "a local access without its slot")),
    }
}

fn error(at: &At, message: impl Into<String>) -> AnalyzerError {
    AnalyzerError {
        index: at.index,
        message: message.into(),
    }
}

impl<V: Value> Frame<V> {
    fn set_local(&mut self, slot: usize, value: V, at: &At) -> Result<(), AnalyzerError> {
        let target = self
            .locals
            .get_mut(slot)
            .ok_or_else(|| error(at, format!("local {slot} past max_locals")))?;
        *target = value;
        Ok(())
    }

    fn pop(&mut self, at: &At) -> Result<V, AnalyzerError> {
        self.stack
            .pop()
            .ok_or_else(|| error(at, "cannot pop an empty operand stack"))
    }

    fn pop_word(&mut self, at: &At) -> Result<V, AnalyzerError> {
        let value = self.pop(at)?;
        if value.size() != 1 {
            return Err(error(at, "a one-word value was expected"));
        }
        Ok(value)
    }

    fn push(&mut self, value: V) {
        self.stack.push(value);
    }

    fn push_result(&mut self, value: Option<V>, at: &At) -> Result<(), AnalyzerError> {
        let value = value.ok_or_else(|| error(at, "the interpreter produced no value"))?;
        self.push(value);
        Ok(())
    }

    /// Meet with an edge arriving in `other`; `true` when anything changed (ASM's `Frame.merge`).
    pub(crate) fn merge<I: Interpreter<V = V>>(
        &mut self,
        other: &Frame<V>,
        interpreter: &mut I,
    ) -> Result<bool, String> {
        if self.stack.len() != other.stack.len() {
            return Err("incompatible stack heights".to_string());
        }
        let mut changed = false;
        for (mine, theirs) in self
            .locals
            .iter_mut()
            .chain(self.stack.iter_mut())
            .zip(other.locals.iter().chain(other.stack.iter()))
        {
            let met = interpreter.merge(mine, theirs);
            if met != *mine {
                *mine = met;
                changed = true;
            }
        }
        Ok(changed)
    }

    /// ASM's `Frame.execute`.
    pub(crate) fn execute<I: Interpreter<V = V>>(
        &mut self,
        at: &At,
        interpreter: &mut I,
    ) -> Result<(), AnalyzerError> {
        let op = opcode(at.insn);
        match op {
            NOP | GOTO => {}
            ACONST_NULL..=LDC2_W | GETSTATIC | NEW => {
                let value = interpreter.new_operation(at)?;
                self.push(value);
            }
            ILOAD..=ALOAD => {
                let slot = var_slot(at)?;
                let local = self
                    .locals
                    .get(slot)
                    .ok_or_else(|| error(at, format!("local {slot} past max_locals")))?
                    .clone();
                let value = interpreter.copy_operation(at, &local)?;
                self.push(value);
            }
            ISTORE..=ASTORE => {
                let slot = var_slot(at)?;
                let popped = self.pop(at)?;
                let value = interpreter.copy_operation(at, &popped)?;
                let wide = value.size() == 2;
                self.set_local(slot, value, at)?;
                if wide {
                    let empty = interpreter.new_empty_value();
                    self.set_local(slot + 1, empty, at)?;
                }
                if slot > 0 && self.locals[slot - 1].size() == 2 {
                    let empty = interpreter.new_empty_value();
                    self.set_local(slot - 1, empty, at)?;
                }
            }
            IALOAD..=SALOAD | IADD..=DREM | ISHL..=LXOR | LCMP..=DCMPG | IF_ICMPEQ..=IF_ACMPNE => {
                let second = self.pop(at)?;
                let first = self.pop(at)?;
                let result = interpreter.binary_operation(at, &first, &second)?;
                if !(IF_ICMPEQ..=IF_ACMPNE).contains(&op) {
                    self.push_result(result, at)?;
                }
            }
            IASTORE..=SASTORE => {
                let third = self.pop(at)?;
                let second = self.pop(at)?;
                let first = self.pop(at)?;
                interpreter.ternary_operation(at, &first, &second, &third)?;
            }
            POP => {
                self.pop_word(at)?;
            }
            POP2 => {
                if self.pop(at)?.size() == 1 {
                    self.pop_word(at)?;
                }
            }
            DUP => {
                let value = self.pop_word(at)?;
                let copy = interpreter.copy_operation(at, &value)?;
                self.push(value);
                self.push(copy);
            }
            DUP_X1 => {
                let first = self.pop_word(at)?;
                let second = self.pop_word(at)?;
                let copy = interpreter.copy_operation(at, &first)?;
                self.push(copy);
                self.push(second);
                self.push(first);
            }
            DUP_X2 => {
                let first = self.pop_word(at)?;
                let second = self.pop(at)?;
                let copy = interpreter.copy_operation(at, &first)?;
                if second.size() == 1 {
                    let third = self.pop_word(at)?;
                    self.push(copy);
                    self.push(third);
                } else {
                    self.push(copy);
                }
                self.push(second);
                self.push(first);
            }
            DUP2 => {
                let first = self.pop(at)?;
                if first.size() == 1 {
                    let second = self.pop_word(at)?;
                    let copy_second = interpreter.copy_operation(at, &second)?;
                    let copy_first = interpreter.copy_operation(at, &first)?;
                    self.push(second);
                    self.push(first);
                    self.push(copy_second);
                    self.push(copy_first);
                } else {
                    let copy = interpreter.copy_operation(at, &first)?;
                    self.push(first);
                    self.push(copy);
                }
            }
            DUP2_X1 => {
                let first = self.pop(at)?;
                if first.size() == 1 {
                    let second = self.pop_word(at)?;
                    let third = self.pop_word(at)?;
                    let copy_second = interpreter.copy_operation(at, &second)?;
                    let copy_first = interpreter.copy_operation(at, &first)?;
                    self.push(copy_second);
                    self.push(copy_first);
                    self.push(third);
                    self.push(second);
                    self.push(first);
                } else {
                    let second = self.pop_word(at)?;
                    let copy = interpreter.copy_operation(at, &first)?;
                    self.push(copy);
                    self.push(second);
                    self.push(first);
                }
            }
            DUP2_X2 => {
                let first = self.pop(at)?;
                if first.size() == 1 {
                    let second = self.pop_word(at)?;
                    let third = self.pop(at)?;
                    let copy_second = interpreter.copy_operation(at, &second)?;
                    let copy_first = interpreter.copy_operation(at, &first)?;
                    if third.size() == 1 {
                        let fourth = self.pop_word(at)?;
                        self.push(copy_second);
                        self.push(copy_first);
                        self.push(fourth);
                    } else {
                        self.push(copy_second);
                        self.push(copy_first);
                    }
                    self.push(third);
                    self.push(second);
                    self.push(first);
                } else {
                    let second = self.pop(at)?;
                    let copy = interpreter.copy_operation(at, &first)?;
                    if second.size() == 1 {
                        let third = self.pop_word(at)?;
                        self.push(copy);
                        self.push(third);
                    } else {
                        self.push(copy);
                    }
                    self.push(second);
                    self.push(first);
                }
            }
            SWAP => {
                let second = self.pop_word(at)?;
                let first = self.pop_word(at)?;
                let copy_second = interpreter.copy_operation(at, &second)?;
                let copy_first = interpreter.copy_operation(at, &first)?;
                self.push(copy_second);
                self.push(copy_first);
            }
            INEG..=DNEG
            | I2L..=I2S
            | GETFIELD
            | NEWARRAY
            | ANEWARRAY
            | ARRAYLENGTH
            | CHECKCAST
            | INSTANCEOF => {
                let value = self.pop(at)?;
                let result = interpreter.unary_operation(at, &value)?;
                self.push_result(result, at)?;
            }
            IINC => {
                let Insn::Iinc { slot, .. } = at.insn else {
                    return Err(error(at, "an iinc without its operands"));
                };
                let slot = usize::from(*slot);
                let local = self
                    .locals
                    .get(slot)
                    .ok_or_else(|| error(at, format!("local {slot} past max_locals")))?
                    .clone();
                let result = interpreter
                    .unary_operation(at, &local)?
                    .ok_or_else(|| error(at, "iinc produced no value"))?;
                self.set_local(slot, result, at)?;
            }
            IFEQ..=IFLE
            | TABLESWITCH
            | LOOKUPSWITCH
            | PUTSTATIC
            | ATHROW
            | MONITORENTER
            | MONITOREXIT
            | IFNULL
            | IFNONNULL => {
                let value = self.pop(at)?;
                interpreter.unary_operation(at, &value)?;
            }
            IRETURN..=ARETURN => {
                let value = self.pop(at)?;
                interpreter.unary_operation(at, &value)?;
                let expected = self
                    .return_value
                    .clone()
                    .ok_or_else(|| error(at, "a value returned from a void method"))?;
                interpreter.return_operation(at, &value, &expected)?;
            }
            RETURN => {
                if self.return_value.is_some() {
                    return Err(error(at, "a void return from a non-void method"));
                }
            }
            PUTFIELD => {
                let second = self.pop(at)?;
                let first = self.pop(at)?;
                interpreter.binary_operation(at, &first, &second)?;
            }
            INVOKEVIRTUAL..=INVOKEDYNAMIC => {
                let descriptor = match at.insn {
                    Insn::Method { desc, .. } | Insn::InvokeDynamic { desc, .. } => desc,
                    _ => return Err(error(at, "an invocation without a descriptor")),
                };
                let arguments = descriptors::argument_types(descriptor)
                    .ok_or_else(|| error(at, "malformed method descriptor"))?
                    .len();
                let receiver = usize::from(op != INVOKESTATIC && op != INVOKEDYNAMIC);
                let mut values = Vec::with_capacity(arguments + receiver);
                for _ in 0..arguments + receiver {
                    values.push(self.pop(at)?);
                }
                values.reverse();
                let result = interpreter.nary_operation(at, &values)?;
                if descriptors::return_type(descriptor) != Some("V") {
                    self.push_result(result, at)?;
                }
            }
            MULTIANEWARRAY => {
                let Insn::MultiANewArray { dims, .. } = at.insn else {
                    return Err(error(at, "a multianewarray without its dimensions"));
                };
                let dimensions = usize::from(*dims);
                let mut values = Vec::with_capacity(dimensions);
                for _ in 0..dimensions {
                    values.push(self.pop(at)?);
                }
                values.reverse();
                let result = interpreter.nary_operation(at, &values)?;
                self.push_result(result, at)?;
            }
            _ => return Err(error(at, format!("unsupported opcode {op:#04x}"))),
        }
        Ok(())
    }
}
