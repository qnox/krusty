//! Declaration-owned instance field coordinates shared by constructor and member bodies.

use super::*;
use crate::ir::{ClassId, IrCheckedOperation};
use crate::libraries::AtomicOperation;
use crate::metadata::klib_ir::tree::KlibIrMemberAccess;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
enum Access {
    Initializer,
    ValueReadOnly,
    /// A member of an ordinary or file-local class: it reads its receiver's fields and writes the
    /// mutable ones.
    Member,
}

#[derive(Clone, Copy)]
struct Field<'a> {
    symbol: &'a KlibIrSymbol,
    index: u32,
    ty: Ty,
    final_: bool,
}

#[derive(Clone)]
pub(super) struct InstanceStorageFrame<'a> {
    class: ClassId,
    receiver: Option<Ty>,
    fields: Vec<Field<'a>>,
    /// A member's class properties with a backing field: the property, then that field.
    properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
    access: Access,
}

impl<'a> InstanceStorageFrame<'a> {
    /// Only constructor initialization storage was inherited by nested bodies. A value member's
    /// read authority remains local to that member, as before the shared frame.
    pub(super) fn nested_initializer(&self) -> Option<Self> {
        matches!(self.access, Access::Initializer).then(|| self.clone())
    }

    pub(super) fn initializer(
        class: ClassId,
        plan: &'a super::super::class_initialization::ClassInitialization<'a, 'a>,
    ) -> Self {
        Self {
            class,
            receiver: Some(super::super::class_headers::declared_receiver(
                plan.header.contract,
            )),
            fields: plan
                .field_declarations
                .iter()
                .enumerate()
                .map(|(index, field)| Field {
                    symbol: &field.base.symbol,
                    index: u32::try_from(index).expect("a field list fits u32"),
                    ty: plan.storage.fields[index].ty,
                    final_: plan.storage.fields[index].final_,
                })
                .collect(),
            properties: Vec::new(),
            access: Access::Initializer,
        }
    }

    pub(super) fn local_initializer(
        class: ClassId,
        receiver: Ty,
        fields: &[super::super::same_library_class::LocalStoredField<'a>],
    ) -> Self {
        Self::local(class, receiver, fields, Access::Initializer)
    }

    pub(super) fn local_member(
        class: ClassId,
        receiver: Ty,
        fields: &[super::super::same_library_class::LocalStoredField<'a>],
    ) -> Self {
        Self::local(class, receiver, fields, Access::Member)
    }

    fn local(
        class: ClassId,
        receiver: Ty,
        fields: &[super::super::same_library_class::LocalStoredField<'a>],
        access: Access,
    ) -> Self {
        Self {
            class,
            receiver: Some(receiver),
            fields: fields
                .iter()
                .enumerate()
                .map(|(index, field)| Field {
                    symbol: field.symbol,
                    index: u32::try_from(index).expect("a field list fits u32"),
                    ty: field.ty,
                    final_: field.final_,
                })
                .collect(),
            properties: Vec::new(),
            access,
        }
    }

    /// A member body of `class`, reading its receiver's fields, each with its declared type and
    /// finality, and the `properties` those fields back. A value class member's one field is its
    /// value, which it never writes.
    pub(super) fn member(
        class: ClassId,
        fields: impl IntoIterator<Item = (&'a KlibIrSymbol, Ty, bool)>,
        properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
        receiver: Option<Ty>,
        value: bool,
    ) -> Self {
        Self {
            class,
            receiver,
            fields: fields
                .into_iter()
                .enumerate()
                .map(|(index, (symbol, ty, final_))| Field {
                    symbol,
                    index: u32::try_from(index).expect("a field list fits u32"),
                    ty,
                    final_,
                })
                .collect(),
            properties,
            access: if value {
                Access::ValueReadOnly
            } else {
                Access::Member
            },
        }
    }

    /// A member of an object literal, whose fields hold its captured values first and then the
    /// `fields` its properties with storage declare, from `first_index`.
    pub(super) fn object_member(
        class: ClassId,
        first_index: u32,
        fields: impl IntoIterator<Item = (&'a KlibIrSymbol, Ty, bool)>,
        properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
        receiver: Ty,
    ) -> Self {
        let mut frame = Self::member(class, fields, properties, Some(receiver), false);
        for field in &mut frame.fields {
            field.index += first_index;
        }
        frame
    }

    fn field(&self, symbol: &KlibIrSymbol) -> Lowered<Field<'a>> {
        let mut matches = self.fields.iter().filter(|field| field.symbol == symbol);
        let Some(field) = matches.next().copied() else {
            return Err(mismatch(match self.access {
                Access::Initializer => {
                    "an initializer accesses a field outside its exact declaring class"
                }
                Access::ValueReadOnly => {
                    "a value class member reads a field outside its declaring class"
                }
                Access::Member => "a class member accesses a field outside its declaring class",
            }));
        };
        if matches.next().is_some() {
            return Err(mismatch(
                "a class field has an invalid or repeated storage identity",
            ));
        }
        Ok(field)
    }
}

impl<'a> BodyLowering<'a> {
    /// A declaration initializer stores through the same exact field projection as decoded access.
    pub(super) fn initializer_store(
        &mut self,
        symbol: &KlibIrSymbol,
        receiver: ExprId,
        value: KlibIrExprId,
    ) -> Lowered<ExprId> {
        let frame = self
            .instance_storage
            .as_ref()
            .expect("initializer frame is installed");
        if !matches!(frame.access, Access::Initializer) {
            return unsupported("a field write");
        }
        let field = frame.field(symbol)?;
        let (class, receiver_ty) = (
            frame.class,
            frame.receiver.expect("initializer receiver is declared"),
        );
        let receiver = self.typed(receiver, receiver_ty);
        let value = self.operand(value, field.ty)?;
        let store = self.ir.add_expr(IrExpr::SetField {
            receiver,
            class,
            index: field.index,
            value,
        });
        Ok(self.typed(store, Ty::Unit))
    }

    pub(super) fn initialized_field(
        &mut self,
        symbol: &KlibIrSymbol,
        receiver: Option<KlibIrExprId>,
        value: Option<KlibIrExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let Some(frame) = self.instance_storage.as_ref() else {
            return unsupported(if value.is_some() {
                "a field write"
            } else {
                "a field read"
            });
        };
        if value.is_some() && matches!(frame.access, Access::ValueReadOnly) {
            return unsupported("a field write");
        }
        let field = frame.field(symbol)?;
        if value.is_some() && matches!(frame.access, Access::Member) && field.final_ {
            return Err(mismatch("a class member writes a final field"));
        }
        let (class, declared_receiver, access) = (frame.class, frame.receiver, frame.access);
        // Preserve the value-member boundary's original error order: type before receiver.
        if matches!(access, Access::ValueReadOnly) && ty != field.ty {
            return Err(mismatch(
                "a field read disagrees with its declared storage type",
            ));
        }
        let declared_receiver = declared_receiver.ok_or_else(|| {
            mismatch(match access {
                Access::ValueReadOnly => "a value class member has no receiver",
                Access::Initializer | Access::Member => "a class member has no receiver",
            })
        })?;
        let receiver =
            receiver.ok_or_else(|| mismatch("an instance field access has no receiver"))?;
        let receiver = self.operand(receiver, declared_receiver)?;
        let lowered = match value {
            Some(value) => {
                if ty != Ty::Unit {
                    return Err(mismatch("a field store is not typed Unit"));
                }
                let value = self.operand(value, field.ty)?;
                self.ir.add_expr(IrExpr::SetField {
                    receiver,
                    class,
                    index: field.index,
                    value,
                })
            }
            None => {
                if ty != field.ty {
                    return Err(mismatch(
                        "a field read disagrees with its declared storage type",
                    ));
                }
                self.ir.add_expr(IrExpr::GetField {
                    receiver,
                    class,
                    index: field.index,
                })
            }
        };
        Ok(self.typed(lowered, ty))
    }

    /// The target stdlib's atomic access `operation` through a reference to a property of this
    /// member's class: the same access to the property's backing field, on the reference's one
    /// bound receiver. The reference is not a value and its accessor lambdas are not called; the
    /// operands follow, in order, after that receiver.
    pub(super) fn atomic_field(
        &mut self,
        operation: AtomicOperation,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let arguments = super::calls::arguments(access);
        let [Some(reference), operands @ ..] = arguments.as_slice() else {
            return Err(mismatch("an atomic field access has no property reference"));
        };
        let reference = *reference;
        let operands = operands.iter().copied().collect::<Option<Vec<_>>>();
        let Some(operands) = operands.filter(|operands| operands.len() == operation.operands())
        else {
            return Err(mismatch(
                "an atomic field access has another shape than its declaration's",
            ));
        };
        let KlibIrExprKind::RichPropertyReference {
            bound_values,
            reflection_target: Some(property),
            setter: Some(_),
            ..
        } = &self.arena.expr(reference).kind
        else {
            return unsupported(
                "an atomic field access through another receiver than a property reference",
            );
        };
        let &[bound] = bound_values.as_slice() else {
            return unsupported(
                "an atomic access through a property reference without one bound receiver",
            );
        };
        let Some(frame) = self
            .instance_storage
            .as_ref()
            .filter(|frame| matches!(frame.access, Access::Member))
        else {
            return unsupported("an atomic field access outside a member of its class");
        };
        let mut backing = frame
            .properties
            .iter()
            .filter(|(declared, _)| *declared == property);
        let (Some(&(_, field)), None) = (backing.next(), backing.next()) else {
            return Err(mismatch(
                "an atomic field access names a property outside its member's class",
            ));
        };
        let field = frame.field(field)?;
        let (class, receiver_ty) = (frame.class, frame.receiver);
        if field.final_ && operation != AtomicOperation::Load {
            return Err(mismatch("an atomic field access writes a final field"));
        }
        let result = match operation {
            AtomicOperation::Store => Ty::Unit,
            AtomicOperation::CompareAndSet => Ty::Boolean,
            AtomicOperation::Load
            | AtomicOperation::Exchange
            | AtomicOperation::CompareAndExchange
            | AtomicOperation::FetchAndAdd => field.ty,
        };
        if ty != result {
            return Err(mismatch(
                "an atomic field access disagrees with its field's storage type",
            ));
        }
        let receiver_ty = receiver_ty.ok_or_else(|| mismatch("a class member has no receiver"))?;
        let receiver = self.operand(bound, receiver_ty)?;
        let operands = operands
            .into_iter()
            .map(|operand| self.operand(operand, field.ty))
            .collect::<Lowered<Vec<_>>>()?;
        let lowered =
            self.ir
                .add_expr(IrExpr::Checked(Box::new(IrCheckedOperation::AtomicField {
                    operation,
                    receiver,
                    class,
                    index: field.index,
                    operands,
                })));
        Ok(self.typed(lowered, ty))
    }
}
