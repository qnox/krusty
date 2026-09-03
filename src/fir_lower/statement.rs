use crate::fir::{
    FirLocalClassCapture, FirLocalClassCaptureSource, FirStatementId, FirStatementKind,
};
use crate::ir::{ExprId, IrCtorArg, IrExpr, IrField};

use super::{BodyLowering, FirLoweringFailure, LoweringState};

impl BodyLowering<'_> {
    pub(super) fn statement(
        &mut self,
        statement_id: FirStatementId,
    ) -> Result<ExprId, FirLoweringFailure> {
        match self.statement_state(statement_id) {
            Some(LoweringState::Lowered(statement)) => return Ok(statement),
            Some(LoweringState::Computing) => {
                return Err(FirLoweringFailure::RecursiveStatement(statement_id));
            }
            Some(LoweringState::Uncomputed) => {}
            None => return Err(FirLoweringFailure::MissingStatement(statement_id)),
        }
        self.set_statement_state(statement_id, LoweringState::Computing);
        let statement = self
            .body
            .statement(statement_id)
            .ok_or(FirLoweringFailure::MissingStatement(statement_id))?;
        let origin = statement.origin;
        let first_generated = self.ir.exprs.len();
        let lowered = match &statement.kind {
            FirStatementKind::Local {
                target,
                ty,
                mutable: _,
                lateinit: _,
                initializer,
                conversion,
            } => {
                let init = initializer
                    .map(|initializer| self.expression_with_conversion(initializer, *conversion))
                    .transpose()?;
                let init = if self.shared_local_type(*target).is_some() {
                    Some(self.shared_cell_new(*ty, init))
                } else {
                    init
                };
                self.ir.add_expr(IrExpr::Variable {
                    index: self.value_slot(*target),
                    ty: ty.get(),
                    init,
                    named: true,
                })
            }
            FirStatementKind::Expression(expression) => self.expression(*expression)?,
            FirStatementKind::Destructure {
                initializer,
                entries,
            } => {
                let initializer_value = self.expression(*initializer)?;
                let temporary = self.allocate_temporary();
                let initializer_ty = self
                    .body
                    .expr(*initializer)
                    .ok_or(FirLoweringFailure::MissingExpression(*initializer))?
                    .ty
                    .get();
                let mut statements = vec![self.ir.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: initializer_ty,
                    init: Some(initializer_value),
                    named: false,
                })];
                let initializer_read = self.ir.add_expr(IrExpr::GetValue(temporary));
                self.set_expression_state(*initializer, LoweringState::Lowered(initializer_read));
                for entry in entries {
                    match *entry {
                        crate::fir::FirDestructureEntry::Binding {
                            target,
                            ty,
                            component,
                            conversion,
                            ..
                        } => {
                            let component =
                                self.expression_with_conversion(component, conversion)?;
                            statements.push(self.ir.add_expr(IrExpr::Variable {
                                index: self.value_slot(target),
                                ty: ty.get(),
                                init: Some(component),
                                named: true,
                            }));
                        }
                        crate::fir::FirDestructureEntry::Ignored { .. } => {}
                    }
                }
                self.ir.add_expr(IrExpr::Block {
                    stmts: statements,
                    value: None,
                })
            }
            FirStatementKind::Loop {
                target,
                header,
                body,
            } => self.loop_statement(*target, header, *body, origin)?,
            FirStatementKind::ConstructorDelegation(call) => {
                self.checked_constructor_delegation(call)?
            }
            FirStatementKind::InterfaceDelegationInitializer {
                classifier,
                delegation,
                value,
            } => self.interface_delegation_initializer(*classifier, *delegation, *value)?,
            FirStatementKind::LocalTypeAlias => self.ir.add_expr(IrExpr::Block {
                stmts: Vec::new(),
                value: None,
            }),
            FirStatementKind::LocalDeclaration {
                declaration,
                captures,
            } => self.local_class_declaration(*declaration, captures)?,
            FirStatementKind::LocalFunction { .. } => self.ir.add_expr(IrExpr::Block {
                stmts: Vec::new(),
                value: None,
            }),
        };
        self.record_expression_origins(first_generated, lowered, origin);
        self.set_statement_state(statement_id, LoweringState::Lowered(lowered));
        Ok(lowered)
    }
}

impl BodyLowering<'_> {
    fn interface_delegation_initializer(
        &mut self,
        declaration: crate::fir::DeclarationId,
        delegation: u32,
        value: crate::fir::FirExprId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let resolved = self
            .index
            .classifier_header(declaration)
            .and_then(|header| header.interface_delegations.get(delegation as usize))
            .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?;
        if resolved.source
            != crate::fir::ResolvedInterfaceDelegateSource::ConstructorBodyInitializer
        {
            return Err(FirLoweringFailure::MissingLocalClass(declaration));
        }
        let class = self
            .ir
            .checked_classifier_classes
            .get(&declaration)
            .copied()
            .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?;
        let field = self
            .ir
            .checked_interface_delegation_fields
            .get(&(declaration, delegation))
            .copied()
            .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?;
        let value_ty = self
            .body
            .expr(value)
            .ok_or(FirLoweringFailure::MissingExpression(value))?
            .ty
            .get();
        let value = self.expression(value)?;
        self.ir.classes[class as usize].fields[field as usize].ty = value_ty;
        if !self
            .ir
            .checked_interface_delegation_initializers
            .insert((declaration, delegation))
        {
            return Err(FirLoweringFailure::MissingLocalClass(declaration));
        }
        let receiver = self.ir.add_expr(IrExpr::GetValue(0));
        Ok(self.ir.add_expr(IrExpr::SetField {
            receiver,
            class,
            index: field,
            value,
        }))
    }

    fn local_class_declaration(
        &mut self,
        declaration: crate::fir::DeclarationId,
        captures: &[FirLocalClassCapture],
    ) -> Result<ExprId, FirLoweringFailure> {
        let (classifier, values) = self.prepare_captured_class(declaration, captures)?;
        self.local_class_captures.insert(classifier, values);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        }))
    }

    pub(super) fn prepare_captured_class(
        &mut self,
        declaration: crate::fir::DeclarationId,
        captures: &[FirLocalClassCapture],
    ) -> Result<(crate::types::TypeName, Vec<(ExprId, crate::types::Ty)>), FirLoweringFailure> {
        let classifier = self
            .index
            .classifier_header(declaration)
            .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?
            .classifier;
        let class = self
            .ir
            .checked_classifier_classes
            .get(&declaration)
            .copied()
            .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?;
        if self.local_class_captures.contains_key(&classifier) {
            return Err(FirLoweringFailure::MissingLocalClass(declaration));
        }
        let mut values = Vec::with_capacity(captures.len());
        let mut fields = Vec::with_capacity(captures.len());
        let mut arguments = Vec::with_capacity(captures.len());
        for (field, capture) in captures.iter().enumerate() {
            let value = match &capture.source {
                FirLocalClassCaptureSource::Value(source) => {
                    self.ir.add_expr(IrExpr::GetValue(self.value_slot(*source)))
                }
                FirLocalClassCaptureSource::Captured {
                    enclosing_depth,
                    source,
                } => self.captured_value(*enclosing_depth, *source)?,
                FirLocalClassCaptureSource::ClassStorage {
                    owner,
                    enclosing_depth,
                    field,
                } => self.enclosing_class_storage_read(*owner, *enclosing_depth, *field)?,
                FirLocalClassCaptureSource::CapturedClassStorage {
                    owner,
                    receiver,
                    path,
                    field,
                } => self.captured_class_storage_holder(*owner, *receiver, path, *field)?,
                FirLocalClassCaptureSource::DispatchReceiver => {
                    let slot = self
                        .dispatch_receiver_slot()
                        .ok_or(FirLoweringFailure::MissingLocalClass(declaration))?;
                    self.ir.add_expr(IrExpr::GetValue(slot))
                }
                FirLocalClassCaptureSource::EnclosingReceiver { path } => {
                    self.enclosing_receiver(path, capture.origin)?
                }
                FirLocalClassCaptureSource::CapturedImplicitReceiver {
                    enclosing_depth,
                    current,
                    depth,
                    path,
                } => {
                    let slot = self
                        .implicit_receiver_capture_slot(*enclosing_depth, *current, *depth, path)
                        .ok_or(FirLoweringFailure::MissingImplicitReceiver {
                            origin: capture.origin,
                        })?;
                    self.ir.add_expr(IrExpr::GetValue(slot))
                }
                FirLocalClassCaptureSource::ImplicitReceiver { current, depth } => {
                    let slot = self.implicit_receiver_slot(*current, *depth).ok_or(
                        FirLoweringFailure::MissingImplicitReceiver {
                            origin: capture.origin,
                        },
                    )?;
                    self.ir.add_expr(IrExpr::GetValue(slot))
                }
            };
            values.push((value, capture.ty.get()));
            fields
                .push(IrField::new(capture.name.to_string(), capture.ty.get()).with_is_final(true));
            arguments.push(IrCtorArg {
                name: Some(capture.name.to_string()),
                ty: capture.ty.get(),
                declared_ty: None,
                is_field: true,
                has_default: false,
                is_vararg: false,
                type_param: None,
                check: capture
                    .ty
                    .get()
                    .is_reference()
                    .then(|| capture.name.to_string())
                    .filter(|_| !capture.ty.get().is_nullable()),
            });
            if capture.shared_cell {
                self.ir.shared_class_capture_fields.insert(
                    (
                        class,
                        u32::try_from(field).expect("too many captured class fields"),
                    ),
                    capture.ty.get(),
                );
            }
        }
        let class = &mut self.ir.classes[class as usize];
        class.fields.splice(0..0, fields);
        class.ctor_args.splice(0..0, arguments);
        class.ctor_param_count += captures.len() as u32;
        class.constructor_prefix_count += captures.len() as u32;
        class.is_local_class = true;
        Ok((classifier, values))
    }
}
