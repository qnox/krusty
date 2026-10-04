//! Checked FIR for `for (v in iterable)` loops: the header chosen for what the iterable is, in
//! the order of kotlinc's `HeaderInfoBuilder`.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn for_each_statement(
        &mut self,
        statement: StmtId,
        origin: OriginId,
        name: &str,
        iterable: ExprId,
        body: ExprId,
        label: &Option<String>,
    ) -> Result<FirStatementKind, BodyCheckFailure> {
        let target = self.body.add_control_target(FirControlTarget {
            origin,
            kind: FirControlTargetKind::Loop,
        });
        let iteration_ty = self.info.semantic_ty(iterable).platform_lower_bound();
        let element_ty = iteration_ty
            .array_read_elem()
            .or((iteration_ty == Ty::String).then_some(Ty::Char))
            .or_else(|| {
                self.info
                    .iterator_protocol(iterable)
                    .map(|protocol| protocol.elem_ty)
            })
            .ok_or_else(|| {
                self.failure(
                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::ForEach),
                )
            })?;
        let element_ty = self.resolved_type(
            self.file
                .stmt_spans
                .get(statement.0 as usize)
                .copied()
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            element_ty,
        )?;
        let variable = self.loop_variable(statement, name);
        let mut body = self.checked_loop_body(
            target,
            label,
            Some((
                name,
                LocalBinding {
                    value: variable,
                    ty: element_ty,
                    lateinit: false,
                },
            )),
            body,
        )?;
        let iterable_expression = self.expression(iterable)?;
        let header = if let Some((header, consumed)) =
            self.with_index_loop_header(statement, variable, iterable, iterable_expression, body)?
        {
            body = consumed;
            header
        } else if iteration_ty.array_elem().is_some() || iteration_ty == Ty::String {
            FirLoopHeader::Iterable {
                variable,
                variable_ty: element_ty,
                kind: if iteration_ty == Ty::String {
                    FirBuiltinIterableKind::String
                } else {
                    FirBuiltinIterableKind::Array
                },
                iterable: iterable_expression,
            }
        } else if let Some(header) =
            self.progression_loop_header(variable, element_ty, iterable, iterable_expression)?
        {
            header
        } else {
            self.iterator_loop_header(
                statement,
                iterable,
                variable,
                element_ty,
                iterable_expression,
            )?
        };
        Ok(FirStatementKind::Loop {
            target,
            header,
            body,
        })
    }
}
