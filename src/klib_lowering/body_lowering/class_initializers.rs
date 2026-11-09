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
            self.ir.classes[class as usize].super_args = arguments;
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

    /// Field stores and anonymous initializers of a same-library file-local class, in member order.
    pub(in super::super) fn initialize_local(
        mut self,
        plan: &super::super::same_library_class::LocalInitialize<'a>,
    ) -> Lowered<ExprId> {
        self.instance_storage = Some(
            super::instance_storage::InstanceStorageFrame::local_initializer(
                plan.class,
                plan.receiver_ty,
                &plan.fields,
            ),
        );
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
            member.receiver,
            &member.fields,
        ));
        self
    }
}
