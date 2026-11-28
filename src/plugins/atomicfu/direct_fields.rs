//! Wasm and Native realizations of a class's `Int` and `Long` atomics.
//!
//! Wasm atomicfu stores the value in an ordinary field and updates it with a plain
//! read-modify-write, which is what its runtime does (`AtomicInt.incrementAndGet` is `++value`).
//! Native stores the same value and updates it with a single atomic instruction, the checked
//! atomic field access the target realizes. The compilation target selects which of those
//! representations runs.
//!
//! Every operation captures its receiver and its arguments, in call order, before it reads or
//! writes the field. `compareAndSet(0, nextValue())` therefore calls `nextValue` when the
//! comparison fails, and `addAndGet` / `getAndSet` observe a field an argument has already
//! mutated.

use super::source_facts::{
    AtomicOp, AtomicValue, DirectAtomic, DirectOperation, FieldRepresentation,
};
use crate::ir::{IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrFile};
use crate::libraries::AtomicOperation;
use crate::types::Ty;

pub(super) fn rewrite(
    ir: &mut IrFile,
    slots: &[DirectAtomic],
    operations: &[DirectOperation],
    representation: FieldRepresentation,
) {
    for slot in slots {
        retarget_field(ir, slot, representation);
    }
    // Reserve above every existing parameter, declaration and use in this IR file.
    let mut temp = ir.next_value_slot();
    for operation in operations {
        let mut statements = Vec::new();
        let value = Lowering {
            ir,
            temp: &mut temp,
            statements: &mut statements,
            operation,
            representation,
        }
        .emit();
        ir.exprs[operation.expression as usize] = IrExpr::Block {
            stmts: statements,
            value,
        };
    }
}

fn retarget_field(ir: &mut IrFile, slot: &DirectAtomic, representation: FieldRepresentation) {
    let storage = &mut ir.classes[slot.class as usize].fields[slot.field as usize];
    storage.ty = slot.value.ty();
    storage.flags = storage.flags.with_is_final(false);
    if representation == FieldRepresentation::NativeAtomic {
        storage.flags = storage.flags.with_is_atomic(true);
    }
    let (store, initial) = slot.initializer;
    if let IrExpr::SetField { value, .. } = &mut ir.exprs[store as usize] {
        *value = initial;
    }
}

/// One operation, after its receiver and arguments have been named.
struct Lowering<'a> {
    ir: &'a mut IrFile,
    temp: &'a mut u32,
    statements: &'a mut Vec<u32>,
    operation: &'a DirectOperation,
    representation: FieldRepresentation,
}

impl Lowering<'_> {
    /// Receiver, then each argument, then the field operation.
    fn emit(mut self) -> Option<u32> {
        let class_ty = Ty::obj_name(self.ir.classes[self.operation.class as usize].fq_name);
        let value_ty = self.operation.value.ty();
        let receiver = self.operation.instance;
        let instance = self.capture(receiver, class_ty);
        let source_arguments = self.operation.arguments.clone();
        let arguments = source_arguments
            .into_iter()
            .map(|argument| self.capture(argument, value_ty))
            .collect::<Vec<_>>();
        match self.operation.op {
            AtomicOp::Get => Some(self.read(instance)),
            AtomicOp::Set => {
                let stored = self.write(instance, arguments[0]);
                self.statements.push(stored);
                None
            }
            AtomicOp::CompareAndSet => {
                Some(self.compare_and_set(instance, arguments[0], arguments[1]))
            }
            AtomicOp::GetAndSet => Some(self.exchange(instance, arguments[0])),
            AtomicOp::GetAndAdd => Some(self.adjust(instance, arguments[0], false)),
            AtomicOp::AddAndGet => Some(self.adjust(instance, arguments[0], true)),
            AtomicOp::GetAndIncrement => Some(self.step(instance, false, true)),
            AtomicOp::IncrementAndGet => Some(self.step(instance, true, true)),
            AtomicOp::GetAndDecrement => Some(self.step(instance, false, false)),
            AtomicOp::DecrementAndGet => Some(self.step(instance, true, false)),
        }
    }

    fn compare_and_set(&mut self, instance: u32, expected: u32, update: u32) -> u32 {
        match self.representation {
            FieldRepresentation::WasmValue => {
                let current = self.read(instance);
                let equal =
                    self.ir
                        .add_expr(IrExpr::primitive_bin_op(IrBinOp::Eq, current, expected));
                let store = self.write(instance, update);
                let yes = self.bool(true);
                let then_branch = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![store],
                    value: Some(yes),
                });
                let no = self.bool(false);
                let decision = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(equal), then_branch), (None, no)],
                });
                self.ir.set_logical_type(decision, Ty::Boolean);
                decision
            }
            FieldRepresentation::NativeAtomic => self.atomic(
                AtomicOperation::CompareAndSet,
                instance,
                vec![expected, update],
                Ty::Boolean,
            ),
        }
    }

    fn exchange(&mut self, instance: u32, update: u32) -> u32 {
        match self.representation {
            FieldRepresentation::WasmValue => {
                let loaded = self.read(instance);
                let previous = self.capture(loaded, self.operation.value.ty());
                let stored = self.write(instance, update);
                self.statements.push(stored);
                previous
            }
            FieldRepresentation::NativeAtomic => {
                let value_ty = self.operation.value.ty();
                self.atomic(AtomicOperation::Exchange, instance, vec![update], value_ty)
            }
        }
    }

    fn adjust(&mut self, instance: u32, delta: u32, returns_new: bool) -> u32 {
        self.update(instance, delta, IrBinOp::Add, returns_new)
    }

    /// An increment adds one. A decrement subtracts one from a plain field and adds minus one
    /// atomically, as the atomic instruction only adds.
    fn step(&mut self, instance: u32, returns_new: bool, increment: bool) -> u32 {
        let (delta, op) = match (increment, self.representation) {
            (true, _) => (self.unit(1), IrBinOp::Add),
            (false, FieldRepresentation::WasmValue) => (self.unit(1), IrBinOp::Sub),
            (false, FieldRepresentation::NativeAtomic) => (self.unit(-1), IrBinOp::Add),
        };
        self.update(instance, delta, op, returns_new)
    }

    fn update(&mut self, instance: u32, delta: u32, op: IrBinOp, returns_new: bool) -> u32 {
        let value_ty = self.operation.value.ty();
        match self.representation {
            FieldRepresentation::WasmValue => {
                let loaded = self.read(instance);
                let previous = self.capture(loaded, value_ty);
                let updated = self.ir.add_arithmetic(op, previous, delta, value_ty);
                let stored = self.write(instance, updated);
                self.statements.push(stored);
                if returns_new {
                    updated
                } else {
                    previous
                }
            }
            FieldRepresentation::NativeAtomic => {
                let previous = self.atomic(
                    AtomicOperation::FetchAndAdd,
                    instance,
                    vec![delta],
                    value_ty,
                );
                if returns_new {
                    self.ir.add_arithmetic(op, previous, delta, value_ty)
                } else {
                    previous
                }
            }
        }
    }

    fn capture(&mut self, value: u32, ty: Ty) -> u32 {
        let index = *self.temp;
        *self.temp += 1;
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index,
            ty,
            init: Some(value),
            named: false,
        });
        self.statements.push(declaration);
        self.ir.add_expr(IrExpr::GetValue(index))
    }

    fn read(&mut self, instance: u32) -> u32 {
        let class = self.operation.class;
        let index = self.operation.field;
        self.ir.add_expr(IrExpr::GetField {
            receiver: instance,
            class,
            index,
        })
    }

    fn write(&mut self, instance: u32, value: u32) -> u32 {
        let class = self.operation.class;
        let index = self.operation.field;
        self.ir.add_expr(IrExpr::SetField {
            receiver: instance,
            class,
            index,
            value,
        })
    }

    fn atomic(
        &mut self,
        operation: AtomicOperation,
        instance: u32,
        operands: Vec<u32>,
        result: Ty,
    ) -> u32 {
        let access = self
            .ir
            .add_expr(IrExpr::Checked(Box::new(IrCheckedOperation::AtomicField {
                operation,
                receiver: instance,
                class: self.operation.class,
                index: self.operation.field,
                operands,
            })));
        self.ir.set_logical_type(access, result);
        access
    }

    fn unit(&mut self, sign: i8) -> u32 {
        let value = self.operation.value;
        self.ir.add_expr(match value {
            AtomicValue::Int => IrExpr::Const(IrConst::Int(i32::from(sign))),
            AtomicValue::Long => IrExpr::Const(IrConst::Long(i64::from(sign))),
        })
    }

    fn bool(&mut self, value: bool) -> u32 {
        self.ir.add_expr(IrExpr::Const(IrConst::Boolean(value)))
    }
}

#[cfg(test)]
mod tests;
