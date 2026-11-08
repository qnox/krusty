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
}
