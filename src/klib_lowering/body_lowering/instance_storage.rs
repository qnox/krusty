//! Declaration-owned field coordinates shared by constructor, member and accessor bodies: the
//! instance fields of a class, and the static storage of a top-level property.

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
    /// A lambda inside such a member. It neither reads nor writes a field itself; a property
    /// reference in it may designate one of the member's class fields, on the receiver the
    /// reference binds, for an atomic access the lambda's inline expansion performs.
    Designation,
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
    /// The serialized declaration of `class`, which declares every one of `fields`.
    declaring: &'a KlibIrSymbol,
    receiver: Option<Ty>,
    fields: Vec<Field<'a>>,
    /// A member's class properties with a backing field: the property, then that field.
    properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
    access: Access,
}

impl<'a> InstanceStorageFrame<'a> {
    /// The frame of a lambda inside this body: an initializer's storage, or a member's fields
    /// for designation only ([`Access::Designation`]).
    pub(super) fn nested(&self) -> Option<Self> {
        match self.access {
            Access::Initializer => Some(self.clone()),
            Access::Member => Some(Self {
                access: Access::Designation,
                ..self.clone()
            }),
            Access::ValueReadOnly | Access::Designation => None,
        }
    }

    pub(super) fn initializer(
        class: ClassId,
        plan: &'a super::super::class_initialization::ClassInitialization<'a, 'a>,
    ) -> Self {
        Self {
            class,
            declaring: &plan.header.class.base.symbol,
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
        declaring: &'a KlibIrSymbol,
        receiver: Ty,
        fields: &[super::super::same_library_class::LocalStoredField<'a>],
    ) -> Self {
        Self::local(class, declaring, receiver, fields, Access::Initializer)
    }

    pub(super) fn local_member(
        class: ClassId,
        declaring: &'a KlibIrSymbol,
        receiver: Ty,
        fields: &[super::super::same_library_class::LocalStoredField<'a>],
    ) -> Self {
        Self::local(class, declaring, receiver, fields, Access::Member)
    }

    fn local(
        class: ClassId,
        declaring: &'a KlibIrSymbol,
        receiver: Ty,
        fields: &[super::super::same_library_class::LocalStoredField<'a>],
        access: Access,
    ) -> Self {
        Self {
            class,
            declaring,
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

    /// A member body of `class`, the class `declaring` declares, reading its receiver's fields,
    /// each with its declared type and finality, and the `properties` those fields back. A value
    /// class member's one field is its value, which it never writes.
    pub(super) fn member(
        class: ClassId,
        declaring: &'a KlibIrSymbol,
        fields: impl IntoIterator<Item = (&'a KlibIrSymbol, Ty, bool)>,
        properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
        receiver: Option<Ty>,
        value: bool,
    ) -> Self {
        Self {
            class,
            declaring,
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
        declaring: &'a KlibIrSymbol,
        first_index: u32,
        fields: impl IntoIterator<Item = (&'a KlibIrSymbol, Ty, bool)>,
        properties: Vec<(&'a KlibIrSymbol, &'a KlibIrSymbol)>,
        receiver: Ty,
    ) -> Self {
        let mut frame = Self::member(class, declaring, fields, properties, Some(receiver), false);
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
                Access::Member | Access::Designation => {
                    "a class member accesses a field outside its declaring class"
                }
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

    /// A field access, whose super qualifier, when it has one, names the class through which it
    /// reaches the field. Every field the frame stores is its own class's, so a qualifier names
    /// that class; one naming a superclass reaches storage this frame does not own.
    pub(super) fn initialized_field(
        &mut self,
        symbol: &KlibIrSymbol,
        super_qualifier: Option<&KlibIrSymbol>,
        receiver: Option<KlibIrExprId>,
        value: Option<KlibIrExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if receiver.is_none() {
            if let Some(field) = self.linker.static_field(self.ir, symbol)? {
                return self.static_field(field, value, ty);
            }
        }
        let Some(frame) = self.instance_storage.as_ref() else {
            return unsupported(if value.is_some() {
                "a field write"
            } else {
                "a field read"
            });
        };
        if super_qualifier.is_some_and(|qualifier| qualifier != frame.declaring) {
            return unsupported(if value.is_some() {
                "a superclass field write"
            } else {
                "a superclass field read"
            });
        }
        if value.is_some() && matches!(frame.access, Access::ValueReadOnly) {
            return unsupported("a field write");
        }
        if matches!(frame.access, Access::Designation) {
            return unsupported(if value.is_some() {
                "a field write"
            } else {
                "a field read"
            });
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
                Access::Initializer | Access::Member | Access::Designation => {
                    "a class member has no receiver"
                }
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

    /// A read or write of the static storage of a top-level property.
    fn static_field(
        &mut self,
        field: super::StaticField,
        value: Option<KlibIrExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let lowered = match value {
            Some(value) => {
                if ty != Ty::Unit {
                    return Err(mismatch("a field store is not typed Unit"));
                }
                if !field.writable {
                    return Err(mismatch("a body writes a final static field"));
                }
                let value = self.operand(value, field.ty)?;
                self.ir.add_expr(IrExpr::SetStatic {
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
                self.ir.add_expr(IrExpr::GetStatic(field.index))
            }
        };
        Ok(self.typed(lowered, ty))
    }

    /// The body of the function that stores a top-level property's initial value: its serialized
    /// initializer, evaluated once by its file's initialization and stored in its static.
    pub(in super::super) fn initialize_static(
        mut self,
        field: super::StaticField,
        initializer: KlibIrExprId,
    ) -> Lowered<ExprId> {
        let value = self.operand(initializer, field.ty)?;
        let store = self.ir.add_expr(IrExpr::SetStatic {
            index: field.index,
            value,
        });
        self.ir.set_logical_type(store, Ty::Unit);
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![store],
            value: None,
        });
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    /// The target stdlib's atomic access `operation` through a reference to a property of this
    /// member's class: the same access to the property's backing field, on the reference's one
    /// bound receiver. The reference is not a value and its accessor lambdas are not called; the
    /// operands follow, in order, after that receiver. A reference this body does not hold itself
    /// (a lambda's result, in an inline helper) is resolved once inline expansion has replaced it
    /// with the lambda's own reference ([`IrCheckedOperation::AtomicFieldThrough`]).
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
        let reference = self.returned_reference(*reference);
        let operands = operands.iter().copied().collect::<Option<Vec<_>>>();
        let Some(operands) = operands.filter(|operands| operands.len() == operation.operands())
        else {
            return Err(mismatch(
                "an atomic field access has another shape than its declaration's",
            ));
        };
        let reference_ty = self.ty(self.arena.expr(reference), "atomic field reference")?;
        let held = matches!(
            self.arena.expr(reference).kind,
            KlibIrExprKind::RichPropertyReference { .. }
        );
        let reference = if held {
            self.field_designation(self.arena.expr(reference), reference_ty)?
        } else {
            self.operand(reference, reference_ty)?
        };
        // The field's storage type types every operand; until the reference is resolved, the
        // intrinsic's type argument, the referenced property's type, does.
        let field_ty = match self.ir.expr(reference) {
            IrExpr::Checked(operation) => match &**operation {
                IrCheckedOperation::FieldDesignation { ty, .. } => Some(*ty),
                _ => None,
            },
            _ => None,
        };
        let operand_ty = match (field_ty, access.type_arguments.as_slice()) {
            (Some(field_ty), _) => field_ty,
            (None, &[Some(property)]) => self.semantic(property)?,
            (None, _) => {
                return Err(mismatch(
                    "an atomic field access does not apply its property's type",
                ))
            }
        };
        let operands = operands
            .into_iter()
            .map(|operand| self.operand(operand, operand_ty))
            .collect::<Lowered<Vec<_>>>()?;
        let through = self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::AtomicFieldThrough {
                operation,
                reference,
                operands,
            },
        )));
        let through = self.typed(through, ty);
        if field_ty.is_some() {
            return super::super::atomic_references::resolve(self.ir, through).map(|()| through);
        }
        Ok(through)
    }

    /// The field a property reference to a property of this body's class designates, on the
    /// one receiver the reference binds: the only use a library body makes of such a reference
    /// is as the receiver of an atomic access, directly or as a lambda's result.
    pub(super) fn field_designation(&mut self, reference: &KlibIrExpr, ty: Ty) -> Lowered<ExprId> {
        let KlibIrExprKind::RichPropertyReference {
            bound_values,
            reflection_target: Some(property),
            setter: Some(_),
            ..
        } = &reference.kind
        else {
            return unsupported("a property reference");
        };
        let &[bound] = bound_values.as_slice() else {
            return unsupported(
                "an atomic access through a property reference without one bound receiver",
            );
        };
        let Some(frame) = self
            .instance_storage
            .as_ref()
            .filter(|frame| matches!(frame.access, Access::Member | Access::Designation))
        else {
            return unsupported("a property reference outside a member of its class");
        };
        let mut backing = frame
            .properties
            .iter()
            .filter(|(declared, _)| *declared == property);
        let (Some(&(_, field)), None) = (backing.next(), backing.next()) else {
            return Err(mismatch(
                "a property reference names a property outside its member's class",
            ));
        };
        let field = frame.field(field)?;
        let (class, receiver_ty) = (frame.class, frame.receiver);
        let receiver_ty = receiver_ty.ok_or_else(|| mismatch("a class member has no receiver"))?;
        let receiver = self.operand(bound, receiver_ty)?;
        let designation = self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::FieldDesignation {
                receiver,
                class,
                index: field.index,
                ty: field.ty,
                final_: field.final_,
            },
        )));
        Ok(self.typed(designation, ty))
    }

    /// The expression `reference` stands for: itself, or, for a returnable block whose only
    /// statement returns a value to it, directly or as the only statement of the inlined function
    /// block it holds, that value. A library that inlines before it serializes leaves a lambda
    /// returning a property reference as such a block, which `returnable_block` lowers to the
    /// returned value alone.
    fn returned_reference(&self, mut reference: KlibIrExprId) -> KlibIrExprId {
        loop {
            let KlibIrExprKind::ReturnableBlock {
                symbol, statements, ..
            } = &self.arena.expr(reference).kind
            else {
                return reference;
            };
            let mut statements = statements.as_slice();
            let returned = loop {
                let [KlibIrStatement::Expression(only)] = statements else {
                    return reference;
                };
                match &self.arena.expr(*only).kind {
                    KlibIrExprKind::InlinedFunctionBlock {
                        statements: inlined,
                        origin: None,
                        ..
                    } => statements = inlined,
                    KlibIrExprKind::Return { target, value } if target == symbol => break *value,
                    _ => return reference,
                }
            };
            reference = returned;
        }
    }
}
