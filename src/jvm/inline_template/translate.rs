//! One instruction at a time: from operand-stack words to operations over values.

use crate::jvm::bytecode_passes::analysis::BasicValue;
use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::bytecode_passes::opcodes::*;
use crate::jvm::method_node::{Category, Constant, Insn, LabelId};

use super::model::{
    ArrayElement, BinaryOp, BlockId, Comparison, Condition, FieldRef, InvokeKind, JvmType, Local,
    LocalId, MethodRef, Op, OpKind, Value, ValueDef, ValueId,
};
use super::DecodeError;

const IF_ICMPLE: u8 = 0xa4;
const IF_ACMPEQ: u8 = 0xa5;
const FCMPL: u8 = 0x95;
const FCMPG: u8 = 0x96;
const DCMPL: u8 = 0x97;

/// The values and locals of the body being decoded.
#[derive(Default)]
pub(super) struct BodyState {
    pub(super) values: Vec<Value>,
    pub(super) locals: Vec<Local>,
}

impl BodyState {
    pub(super) fn new_value(&mut self, ty: JvmType, def: ValueDef) -> ValueId {
        let id = ValueId(self.values.len() as u32);
        self.values.push(Value { ty, def });
        id
    }

    /// The local for `slot` used as `category`, created on first use.
    pub(super) fn local(&mut self, slot: u16, category: Category) -> LocalId {
        let wanted = Local { slot, category };
        if let Some(index) = self.locals.iter().position(|local| *local == wanted) {
            return LocalId(index as u32);
        }
        self.locals.push(wanted);
        LocalId(self.locals.len() as u32 - 1)
    }

    fn ty(&self, value: ValueId) -> &JvmType {
        &self.values[value.0 as usize].ty
    }
}

/// How an instruction leaves its block, with its targets still labels.
pub(super) enum Exit {
    Goto(LabelId),
    Branch {
        condition: Condition,
        taken: LabelId,
    },
    Switch {
        value: ValueId,
        cases: Vec<(i32, LabelId)>,
        default: LabelId,
    },
    Return(Option<ValueId>),
    Throw(ValueId),
}

/// A block being decoded: its operations so far and the values on the operand stack.
pub(super) struct BlockTranslator<'a> {
    pub(super) state: &'a mut BodyState,
    pub(super) block: BlockId,
    pub(super) stack: Vec<ValueId>,
    pub(super) ops: Vec<Op>,
    /// The node being translated, for errors.
    pub(super) node: usize,
    pub(super) line: Option<u16>,
}

/// The [`JvmType`] of an analyzed value; `None` for an unassigned slot or a meet of incompatible
/// values.
pub(super) fn jvm_type(value: &BasicValue) -> Option<JvmType> {
    Some(match value {
        BasicValue::Uninitialized => return None,
        BasicValue::Int => JvmType::Int,
        BasicValue::Float => JvmType::Float,
        BasicValue::Long => JvmType::Long,
        BasicValue::Double => JvmType::Double,
        BasicValue::Boolean => JvmType::Boolean,
        BasicValue::Char => JvmType::Char,
        BasicValue::Byte => JvmType::Byte,
        BasicValue::Short => JvmType::Short,
        BasicValue::Reference(descriptor) => JvmType::Reference((*descriptor).to_string()),
        BasicValue::Null => JvmType::Null,
    })
}

const OBJECT: &str = "Ljava/lang/Object;";

fn reference(descriptor: &str) -> JvmType {
    JvmType::Reference(descriptor.to_string())
}

fn constant_type(constant: &Constant) -> JvmType {
    match constant {
        Constant::Int(_) => JvmType::Int,
        Constant::Float(_) => JvmType::Float,
        Constant::Long(_) => JvmType::Long,
        Constant::Double(_) => JvmType::Double,
        Constant::String(_) => reference("Ljava/lang/String;"),
        Constant::Class(_) => reference("Ljava/lang/Class;"),
        Constant::MethodType(_) => reference("Ljava/lang/invoke/MethodType;"),
        Constant::Handle(_) => reference("Ljava/lang/invoke/MethodHandle;"),
    }
}

/// The category an arithmetic opcode works on, from its offset in a four-opcode family.
fn family_category(base: u8, op: u8) -> Category {
    [
        Category::Int,
        Category::Long,
        Category::Float,
        Category::Double,
    ][usize::from((op - base) % 4)]
}

fn comparison(offset: u8) -> Comparison {
    [
        Comparison::Eq,
        Comparison::Ne,
        Comparison::Lt,
        Comparison::Ge,
        Comparison::Gt,
        Comparison::Le,
    ][usize::from(offset)]
}

fn array_element(offset: u8) -> ArrayElement {
    [
        ArrayElement::Int,
        ArrayElement::Long,
        ArrayElement::Float,
        ArrayElement::Double,
        ArrayElement::Reference,
        ArrayElement::ByteOrBoolean,
        ArrayElement::Char,
        ArrayElement::Short,
    ][usize::from(offset)]
}

/// The element descriptor of a `newarray` type code (JVMS 6.5.newarray).
fn primitive_array_element(code: i32) -> Option<&'static str> {
    Some(match code {
        4 => "Z",
        5 => "C",
        6 => "F",
        7 => "D",
        8 => "B",
        9 => "S",
        10 => "I",
        11 => "J",
        _ => return None,
    })
}

/// `i2l` … `i2s`: the operand's category and the result's type.
fn conversion(op: u8) -> (Category, JvmType) {
    match op {
        I2L => (Category::Int, JvmType::Long),
        I2F => (Category::Int, JvmType::Float),
        I2D => (Category::Int, JvmType::Double),
        L2I => (Category::Long, JvmType::Int),
        L2F => (Category::Long, JvmType::Float),
        L2D => (Category::Long, JvmType::Double),
        F2I => (Category::Float, JvmType::Int),
        F2L => (Category::Float, JvmType::Long),
        F2D => (Category::Float, JvmType::Double),
        D2I => (Category::Double, JvmType::Int),
        D2L => (Category::Double, JvmType::Long),
        D2F => (Category::Double, JvmType::Float),
        I2B => (Category::Int, JvmType::Byte),
        I2C => (Category::Int, JvmType::Char),
        _ => (Category::Int, JvmType::Short),
    }
}

impl BlockTranslator<'_> {
    fn error(&self, reason: &'static str) -> DecodeError {
        DecodeError::malformed(self.node, reason)
    }

    fn pop(&mut self) -> Result<ValueId, DecodeError> {
        self.stack
            .pop()
            .ok_or_else(|| self.error("operand stack underflow"))
    }

    /// Pop `count` values, returned bottom first.
    fn pop_values(&mut self, count: usize) -> Result<Vec<ValueId>, DecodeError> {
        let at = self
            .stack
            .len()
            .checked_sub(count)
            .ok_or_else(|| self.error("operand stack underflow"))?;
        Ok(self.stack.split_off(at))
    }

    /// Pop the values that make up the top `words` stack words, bottom first. A two-word value
    /// split by the boundary is malformed.
    fn pop_words(&mut self, words: usize) -> Result<Vec<ValueId>, DecodeError> {
        let mut taken = 0;
        let mut at = self.stack.len();
        while taken < words {
            at = at
                .checked_sub(1)
                .ok_or_else(|| self.error("operand stack underflow"))?;
            taken += self.state.ty(self.stack[at]).category().words() as usize;
        }
        if taken != words {
            return Err(self.error("stack manipulation splits a two-word value"));
        }
        Ok(self.stack.split_off(at))
    }

    fn push_op(&mut self, kind: OpKind, result: Option<JvmType>) -> Option<ValueId> {
        let def = ValueDef::Op {
            block: self.block,
            op: self.ops.len() as u32,
        };
        let result = result.map(|ty| self.state.new_value(ty, def));
        self.ops.push(Op {
            kind,
            result,
            line: self.line,
        });
        if let Some(value) = result {
            self.stack.push(value);
        }
        result
    }

    fn effect(&mut self, kind: OpKind) {
        self.push_op(kind, None);
    }

    fn produce(&mut self, kind: OpKind, ty: JvmType) {
        self.push_op(kind, Some(ty));
    }

    /// Translate one instruction. `locals` is the analyzed frame's locals before it, which type a
    /// local load. Returns how the instruction leaves the block, if it does.
    pub(super) fn translate(
        &mut self,
        insn: &Insn,
        locals: &[BasicValue],
    ) -> Result<Option<Exit>, DecodeError> {
        match insn {
            Insn::Op(op) => return self.plain(*op),
            Insn::Int {
                op: NEWARRAY,
                operand,
            } => {
                let element = primitive_array_element(*operand)
                    .ok_or_else(|| self.error("unknown newarray type code"))?;
                let length = self.pop()?;
                self.produce(
                    OpKind::NewArray {
                        element: element.to_string(),
                        length,
                    },
                    reference(&format!("[{element}")),
                );
            }
            Insn::Int { operand, .. } => {
                self.produce(OpKind::Constant(Constant::Int(*operand)), JvmType::Int)
            }
            Insn::Var { op, slot } => self.variable(*op, *slot, locals)?,
            Insn::Iinc { slot, delta } => {
                let local = self.state.local(*slot, Category::Int);
                self.effect(OpKind::IncrementLocal {
                    local,
                    delta: *delta,
                });
            }
            Insn::Type { op, class } => self.type_instruction(*op, class)?,
            Insn::Field {
                op,
                owner,
                name,
                desc,
            } => self.field(*op, owner, name, desc)?,
            Insn::Method {
                op,
                owner,
                name,
                desc,
                interface,
            } => {
                let kind = match *op {
                    INVOKEVIRTUAL => InvokeKind::Virtual,
                    INVOKESPECIAL => InvokeKind::Special,
                    INVOKESTATIC => InvokeKind::Static,
                    _ => InvokeKind::Interface,
                };
                let target = MethodRef {
                    owner: owner.clone(),
                    name: name.clone(),
                    descriptor: desc.clone(),
                    interface: *interface,
                };
                self.invoke(desc, kind != InvokeKind::Static, |arguments| {
                    OpKind::Invoke {
                        kind,
                        target,
                        arguments,
                    }
                })?;
            }
            Insn::InvokeDynamic {
                name,
                desc,
                bootstrap,
                arguments,
            } => {
                let bootstrap = bootstrap.clone();
                let bootstrap_arguments = arguments.clone();
                self.invoke(desc, false, |arguments| OpKind::InvokeDynamic {
                    name: name.clone(),
                    descriptor: desc.clone(),
                    bootstrap,
                    bootstrap_arguments,
                    arguments,
                })?;
            }
            Insn::Jump { op, target } => return self.jump(*op, *target).map(Some),
            Insn::Ldc(constant) => {
                self.produce(OpKind::Constant(constant.clone()), constant_type(constant))
            }
            Insn::TableSwitch {
                low,
                default,
                labels,
                ..
            } => {
                let value = self.pop()?;
                let cases = labels
                    .iter()
                    .enumerate()
                    .map(|(offset, label)| (low.wrapping_add(offset as i32), *label))
                    .collect();
                return Ok(Some(Exit::Switch {
                    value,
                    cases,
                    default: *default,
                }));
            }
            Insn::LookupSwitch {
                default,
                keys,
                labels,
            } => {
                let value = self.pop()?;
                let cases = keys.iter().copied().zip(labels.iter().copied()).collect();
                return Ok(Some(Exit::Switch {
                    value,
                    cases,
                    default: *default,
                }));
            }
            Insn::MultiANewArray { desc, dims } => {
                let dimensions = self.pop_values(usize::from(*dims))?;
                self.produce(
                    OpKind::MultiNewArray {
                        descriptor: desc.clone(),
                        dimensions,
                    },
                    reference(desc),
                );
            }
        }
        Ok(None)
    }

    /// An instruction with no operand.
    fn plain(&mut self, op: u8) -> Result<Option<Exit>, DecodeError> {
        match op {
            NOP => {}
            ACONST_NULL => self.produce(OpKind::Null, JvmType::Null),
            ICONST_M1..=ICONST_5 => self.produce(
                OpKind::Constant(Constant::Int(i32::from(op) - i32::from(ICONST_0))),
                JvmType::Int,
            ),
            LCONST_0 | LCONST_1 => self.produce(
                OpKind::Constant(Constant::Long(i64::from(op - LCONST_0))),
                JvmType::Long,
            ),
            FCONST_0..=FCONST_2 => self.produce(
                OpKind::Constant(Constant::Float(f32::from(op - FCONST_0).to_bits())),
                JvmType::Float,
            ),
            DCONST_0 | DCONST_1 => self.produce(
                OpKind::Constant(Constant::Double(f64::from(op - DCONST_0).to_bits())),
                JvmType::Double,
            ),
            IALOAD..=SALOAD => {
                let index = self.pop()?;
                let array = self.pop()?;
                let element = array_element(op - IALOAD);
                let ty = self.loaded_element(element, array);
                self.produce(
                    OpKind::ArrayLoad {
                        element,
                        array,
                        index,
                    },
                    ty,
                );
            }
            IASTORE..=SASTORE => {
                let value = self.pop()?;
                let index = self.pop()?;
                let array = self.pop()?;
                self.effect(OpKind::ArrayStore {
                    element: array_element(op - IASTORE),
                    array,
                    index,
                    value,
                });
            }
            POP => {
                self.pop_words(1)?;
            }
            POP2 => {
                self.pop_words(2)?;
            }
            DUP..=DUP2_X2 => {
                // `dup` family: copy the top one or two words, then insert the copy below the
                // zero, one or two words under them.
                let (copied, under) = match op {
                    DUP => (1, 0),
                    DUP_X1 => (1, 1),
                    DUP_X2 => (1, 2),
                    DUP2 => (2, 0),
                    DUP2_X1 => (2, 1),
                    _ => (2, 2),
                };
                let top = self.pop_words(copied)?;
                let below = self.pop_words(under)?;
                self.stack.extend_from_slice(&top);
                self.stack.extend(below);
                self.stack.extend(top);
            }
            SWAP => {
                let top = self.pop_words(1)?;
                let below = self.pop_words(1)?;
                self.stack.extend(top);
                self.stack.extend(below);
            }
            IADD..=DREM => {
                let category = family_category(IADD, op);
                let binary = [
                    BinaryOp::Add,
                    BinaryOp::Sub,
                    BinaryOp::Mul,
                    BinaryOp::Div,
                    BinaryOp::Rem,
                ][usize::from((op - IADD) / 4)];
                self.binary(binary, category)?;
            }
            INEG..=DNEG => {
                let category = family_category(INEG, op);
                let operand = self.pop()?;
                self.produce(
                    OpKind::Negate { category, operand },
                    category_type(category),
                );
            }
            ISHL..=LXOR => {
                let offset = op - ISHL;
                // Each operator comes as an `int` opcode followed by its `long` one.
                let category = [Category::Int, Category::Long][usize::from(offset % 2)];
                let binary = [
                    BinaryOp::Shl,
                    BinaryOp::Shr,
                    BinaryOp::UShr,
                    BinaryOp::And,
                    BinaryOp::Or,
                    BinaryOp::Xor,
                ][usize::from(offset / 2)];
                self.binary(binary, category)?;
            }
            I2L..=I2S => {
                let (from, to) = conversion(op);
                let operand = self.pop()?;
                self.produce(
                    OpKind::Convert {
                        from,
                        to: to.clone(),
                        operand,
                    },
                    to,
                );
            }
            LCMP..=DCMPG => {
                let (binary, category) = match op {
                    LCMP => (BinaryOp::Compare, Category::Long),
                    FCMPL => (BinaryOp::CompareNanLess, Category::Float),
                    FCMPG => (BinaryOp::CompareNanGreater, Category::Float),
                    DCMPL => (BinaryOp::CompareNanLess, Category::Double),
                    _ => (BinaryOp::CompareNanGreater, Category::Double),
                };
                let rhs = self.pop()?;
                let lhs = self.pop()?;
                self.produce(
                    OpKind::Binary {
                        op: binary,
                        category,
                        lhs,
                        rhs,
                    },
                    JvmType::Int,
                );
            }
            IRETURN..=ARETURN => return Ok(Some(Exit::Return(Some(self.pop()?)))),
            RETURN => return Ok(Some(Exit::Return(None))),
            ARRAYLENGTH => {
                let array = self.pop()?;
                self.produce(OpKind::ArrayLength(array), JvmType::Int);
            }
            ATHROW => return Ok(Some(Exit::Throw(self.pop()?))),
            MONITORENTER => {
                let value = self.pop()?;
                self.effect(OpKind::MonitorEnter(value));
            }
            MONITOREXIT => {
                let value = self.pop()?;
                self.effect(OpKind::MonitorExit(value));
            }
            _ => return Err(self.error("unknown instruction")),
        }
        Ok(None)
    }

    fn binary(&mut self, op: BinaryOp, category: Category) -> Result<(), DecodeError> {
        let rhs = self.pop()?;
        let lhs = self.pop()?;
        self.produce(
            OpKind::Binary {
                op,
                category,
                lhs,
                rhs,
            },
            category_type(category),
        );
        Ok(())
    }

    /// The type an array load produces: the array value's element type where it is known.
    fn loaded_element(&self, element: ArrayElement, array: ValueId) -> JvmType {
        let array_element = match self.state.ty(array) {
            JvmType::Reference(descriptor) => descriptors::element_type(descriptor),
            _ => None,
        };
        match element {
            ArrayElement::Int => JvmType::Int,
            ArrayElement::Long => JvmType::Long,
            ArrayElement::Float => JvmType::Float,
            ArrayElement::Double => JvmType::Double,
            ArrayElement::Char => JvmType::Char,
            ArrayElement::Short => JvmType::Short,
            ArrayElement::ByteOrBoolean if array_element == Some("Z") => JvmType::Boolean,
            ArrayElement::ByteOrBoolean => JvmType::Byte,
            ArrayElement::Reference => reference(
                array_element
                    .filter(|e| descriptors::is_reference(e))
                    .unwrap_or(OBJECT),
            ),
        }
    }

    fn variable(&mut self, op: u8, slot: u16, locals: &[BasicValue]) -> Result<(), DecodeError> {
        match op {
            ILOAD..=ALOAD => {
                let category = family_category_of_load(op - ILOAD);
                let ty = locals
                    .get(usize::from(slot))
                    .and_then(jvm_type)
                    .ok_or_else(|| self.error("load of an unassigned local"))?;
                if ty.category() != category {
                    return Err(self.error("load of a local with another category"));
                }
                let local = self.state.local(slot, category);
                self.produce(OpKind::LoadLocal(local), ty);
            }
            ISTORE..=ASTORE => {
                let category = family_category_of_load(op - ISTORE);
                let value = self.pop()?;
                if self.state.ty(value).category() != category {
                    return Err(self.error("store of a value with another category"));
                }
                let local = self.state.local(slot, category);
                self.effect(OpKind::StoreLocal { local, value });
            }
            _ => return Err(self.error("jsr/ret subroutines are not decoded")),
        }
        Ok(())
    }

    fn type_instruction(&mut self, op: u8, class: &str) -> Result<(), DecodeError> {
        match op {
            NEW => self.produce(
                OpKind::New {
                    class: class.to_string(),
                },
                reference(&descriptors::of_internal_name(class)),
            ),
            ANEWARRAY => {
                let element = descriptors::of_internal_name(class);
                let length = self.pop()?;
                let ty = reference(&format!("[{element}"));
                self.produce(OpKind::NewArray { element, length }, ty);
            }
            CHECKCAST => {
                let value = self.pop()?;
                self.produce(
                    OpKind::CheckCast {
                        class: class.to_string(),
                        value,
                    },
                    reference(&descriptors::of_internal_name(class)),
                );
            }
            _ => {
                let value = self.pop()?;
                self.produce(
                    OpKind::InstanceOf {
                        class: class.to_string(),
                        value,
                    },
                    JvmType::Int,
                );
            }
        }
        Ok(())
    }

    fn field(&mut self, op: u8, owner: &str, name: &str, desc: &str) -> Result<(), DecodeError> {
        let field = FieldRef {
            owner: owner.to_string(),
            name: name.to_string(),
            descriptor: desc.to_string(),
        };
        let ty =
            JvmType::of_descriptor(desc).ok_or_else(|| self.error("malformed field descriptor"))?;
        match op {
            GETSTATIC => self.produce(
                OpKind::GetField {
                    field,
                    object: None,
                },
                ty,
            ),
            GETFIELD => {
                let object = Some(self.pop()?);
                self.produce(OpKind::GetField { field, object }, ty);
            }
            PUTSTATIC => {
                let value = self.pop()?;
                self.effect(OpKind::PutField {
                    field,
                    object: None,
                    value,
                });
            }
            _ => {
                let value = self.pop()?;
                let object = Some(self.pop()?);
                self.effect(OpKind::PutField {
                    field,
                    object,
                    value,
                });
            }
        }
        Ok(())
    }

    /// A call of a method with descriptor `desc`: pop its arguments (and receiver), then push its
    /// result unless it returns `void`.
    fn invoke(
        &mut self,
        desc: &str,
        has_receiver: bool,
        kind: impl FnOnce(Vec<ValueId>) -> OpKind,
    ) -> Result<(), DecodeError> {
        let parameters = descriptors::argument_types(desc)
            .ok_or_else(|| self.error("malformed method descriptor"))?;
        let returned = descriptors::return_type(desc)
            .ok_or_else(|| self.error("malformed method descriptor"))?;
        let arguments = self.pop_values(parameters.len() + usize::from(has_receiver))?;
        let result = match returned {
            "V" => None,
            returned => Some(
                JvmType::of_descriptor(returned)
                    .ok_or_else(|| self.error("malformed method descriptor"))?,
            ),
        };
        self.push_op(kind(arguments), result);
        Ok(())
    }

    fn jump(&mut self, op: u8, target: LabelId) -> Result<Exit, DecodeError> {
        let condition = match op {
            GOTO => return Ok(Exit::Goto(target)),
            IFEQ..=IFLE => Condition::IntZero {
                comparison: comparison(op - IFEQ),
                value: self.pop()?,
            },
            IF_ICMPEQ..=IF_ICMPLE => {
                let rhs = self.pop()?;
                let lhs = self.pop()?;
                Condition::Ints {
                    comparison: comparison(op - IF_ICMPEQ),
                    lhs,
                    rhs,
                }
            }
            IF_ACMPEQ | IF_ACMPNE => {
                let rhs = self.pop()?;
                let lhs = self.pop()?;
                Condition::References {
                    equal: op == IF_ACMPEQ,
                    lhs,
                    rhs,
                }
            }
            IFNULL | IFNONNULL => Condition::Null {
                is_null: op == IFNULL,
                value: self.pop()?,
            },
            _ => return Err(self.error("jsr/ret subroutines are not decoded")),
        };
        Ok(Exit::Branch {
            condition,
            taken: target,
        })
    }
}

/// The category of the `offset`th opcode of a load or store family (`iload`…`aload`).
fn family_category_of_load(offset: u8) -> Category {
    [
        Category::Int,
        Category::Long,
        Category::Float,
        Category::Double,
        Category::Reference,
    ][usize::from(offset)]
}

/// The stack type an arithmetic result of `category` has.
fn category_type(category: Category) -> JvmType {
    match category {
        Category::Int => JvmType::Int,
        Category::Long => JvmType::Long,
        Category::Float => JvmType::Float,
        Category::Double => JvmType::Double,
        Category::Reference => reference(OBJECT),
    }
}
