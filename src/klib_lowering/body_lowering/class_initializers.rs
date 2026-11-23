//! Ordered field stores and anonymous initializer effects in their real constructor frame.

use super::super::class_initialization::{ClassInitialization, InitializationStep};
use super::*;
use crate::ir::ClassId;

impl<'a> BodyLowering<'a> {
    pub(in super::super) fn initialize_class(
        mut self,
        class: ClassId,
        plan: &'a ClassInitialization<'a, 'a>,
    ) -> Lowered<ExprId> {
        self.instance_storage = Some(super::instance_storage::InstanceStorageFrame::initializer(
            class, plan,
        ));
        // The superclass constructor's arguments read the constructor's own parameters, which the
        // initialization frame numbers as the constructor does.
        if let Some(superclass) = &plan.superclass {
            let arguments = superclass
                .arguments
                .iter()
                .map(|&argument| self.expression(argument))
                .collect::<Lowered<Vec<_>>>()?;
            let declaration = &mut self.ir.classes[class as usize];
            if plan.primary {
                declaration.super_args = arguments;
            } else {
                // The secondary constructor this initialization is for declares exactly its
                // parameters; Kotlin declares no two constructors of a class with the same ones.
                let params = self.header.params();
                let secondary = declaration
                    .secondary_ctors
                    .iter_mut()
                    .find(|secondary| secondary.params == params)
                    .ok_or_else(|| {
                        mismatch("a secondary constructor's initialization has no constructor")
                    })?;
                secondary.delegate_args = arguments;
            }
        }
        let mut stmts = Vec::new();
        for step in &plan.steps {
            let statement = match step {
                InitializationStep::Field { index, value } => {
                    let receiver = self.read(
                        &plan
                            .header
                            .class
                            .this_receiver
                            .as_ref()
                            .expect("the plan authenticated its instance receiver")
                            .base
                            .symbol,
                    )?;
                    self.initializer_store(
                        &plan.field_declarations[*index as usize].base.symbol,
                        receiver,
                        *value,
                    )?
                }
                InitializationStep::Block(statements) => self.block(statements)?,
            };
            stmts.push(statement);
        }
        let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    /// A secondary constructor of the materialized `class` delegating to another of its
    /// constructors: the delegation's arguments become the arguments of the class's secondary
    /// constructor `secondary`, then its own statements run as a member of the class does,
    /// reading and writing the receiver's `fields`.
    pub(in super::super) fn delegate_within_class(
        mut self,
        class: ClassId,
        secondary: usize,
        member: &super::super::body_unit::MemberClass<'a>,
        arguments: &[crate::metadata::klib_ir::tree::KlibIrExprId],
        statements: &'a [KlibIrStatement],
    ) -> Lowered<ExprId> {
        let declared = &self.ir.classes[class as usize];
        let receiver = super::super::class_headers::declared_receiver(member.contract);
        let storage = member
            .fields
            .iter()
            .zip(&declared.fields)
            .map(|(field, declared)| (&field.base.symbol, declared.ty, declared.is_final()))
            .collect::<Vec<_>>();
        let crate::ir::CtorDelegateTarget::This { target_params, .. } =
            &declared.secondary_ctors[secondary].delegate
        else {
            return Err(mismatch(
                "a delegation within the class reaches another class's constructor",
            ));
        };
        let target = target_params.clone();
        if target.len() != arguments.len() {
            return Err(mismatch(
                "a delegation within the class passes another number of arguments",
            ));
        }
        self.instance_storage = Some(super::instance_storage::InstanceStorageFrame::member(
            class,
            member.declaring,
            storage,
            member.properties.clone(),
            Some(receiver),
            false,
        ));
        // The arguments read the constructor's own parameters, which this frame numbers as the
        // constructor does.
        let arguments = arguments
            .iter()
            .zip(target)
            .map(|(&argument, ty)| self.operand(argument, ty))
            .collect::<Lowered<Vec<_>>>()?;
        self.ir.classes[class as usize].secondary_ctors[secondary].delegate_args = arguments;
        self.body(statements)
    }

    /// Field stores and anonymous initializers of a same-library file-local class, in member order.
    pub(in super::super) fn initialize_local(
        mut self,
        plan: &super::super::same_library_class::LocalInitialize<'a>,
    ) -> Lowered<ExprId> {
        self.instance_storage = Some(
            super::instance_storage::InstanceStorageFrame::local_initializer(
                plan.class,
                plan.declaring,
                plan.receiver_ty,
                &plan.fields,
            ),
        );
        self.read_outer_instance(plan.class, plan.outer);
        // The superclass constructor's arguments read the constructor's values, which this
        // initialization's frame numbers as the constructor does.
        if !plan.superclass_arguments.is_empty() {
            let arguments = plan
                .superclass_arguments
                .iter()
                .map(|&argument| self.expression(argument))
                .collect::<Lowered<Vec<_>>>()?;
            self.ir.classes[plan.class as usize].super_args = arguments;
        }
        let mut stmts = Vec::new();
        for step in &plan.steps {
            let statement = match step {
                InitializationStep::Field { index, value } => {
                    let receiver = self.read(plan.receiver_symbol)?;
                    self.initializer_store(plan.fields[*index as usize].symbol, receiver, *value)?
                }
                InitializationStep::Block(statements) => self.block(statements)?,
            };
            stmts.push(statement);
        }
        let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    /// A member of a same-library file-local class, reading and writing that class's fields.
    pub(in super::super) fn reading_local_fields(
        mut self,
        member: &super::super::same_library_class::LocalMemberFields<'a>,
    ) -> Self {
        self.instance_storage = Some(super::instance_storage::InstanceStorageFrame::local_member(
            member.class,
            member.declaring,
            member.receiver,
            &member.fields,
        ));
        self.read_outer_instance(member.class, member.outer);
        self
    }

    /// An inner class's bodies read the outer class's `this` from the field its constructor
    /// stores the outer instance in.
    fn read_outer_instance(
        &mut self,
        class: crate::ir::ClassId,
        outer: Option<super::super::same_library_class::OuterInstance<'a>>,
    ) {
        if let Some(outer) = outer {
            let class_name = self.ir.classes[class as usize].fq_name_id();
            self.captures.insert(
                outer.symbol.clone(),
                super::object_literals::CaptureField {
                    class,
                    index: outer.field,
                    class_name,
                },
            );
        }
    }
}
