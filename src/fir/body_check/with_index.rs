//! kotlinc's `WithIndexHandler`: a `for` loop that destructures the `IndexedValue`s of a
//! `withIndex()` call in its header iterates the call's receiver instead, and its destructuring
//! reads the index the loop counts and the element it reads.

use super::*;
use crate::fir::{
    FirBuiltinIterableKind, FirConversionKind, FirDestructureEntry, FirExprKind,
    FirIndexedValueComponent, FirStatementId, FirStatementKind, FirWithIndexLoop, LocalValueId,
};
use crate::libraries::CompilerIntrinsic;
use crate::resolve::ResolvedCall;

impl BodyFirChecker<'_> {
    /// The header of a loop over `receiver.withIndex()` whose variable is destructured in the loop
    /// header, with the loop body that no longer opens with that destructuring. `None` leaves the
    /// loop iterating the `IndexedValue`s: a loop variable declared without destructuring, an
    /// implicit receiver, a receiver no nested loop handles (`NestedHeaderInfoBuilderForWithIndex`),
    /// or an entry that reads neither component.
    pub(super) fn with_index_loop_header(
        &mut self,
        statement: StmtId,
        container: LocalValueId,
        iterable: ExprId,
        checked: FirExprId,
        body: FirExprId,
    ) -> Result<Option<(FirLoopHeader, FirExprId)>, BodyCheckFailure> {
        let Some(&destructure_source) = self.file.destructuring.loops.get(&statement) else {
            return Ok(None);
        };
        let Some(ResolvedCall::Extension(call)) = self.info.resolved_calls.get(&iterable) else {
            return Ok(None);
        };
        if super::calls::selected_extension_intrinsic(call) != Some(CompilerIntrinsic::WithIndex) {
            return Ok(None);
        }
        let Expr::Call { callee, .. } = self.file.expr(iterable) else {
            return Ok(None);
        };
        let Expr::Member {
            receiver: receiver_source,
            ..
        } = self.file.expr(*callee)
        else {
            return Ok(None);
        };
        let receiver_source = *receiver_source;
        let Some(FirExprKind::Call(call)) = self.body.expr(checked).map(|call| &call.kind) else {
            return Ok(None);
        };
        let Some(receiver) = call.extension_receiver.filter(|receiver| {
            receiver.conversion.is_none_or(|conversion| {
                matches!(
                    conversion.kind,
                    FirConversionKind::SmartCast { .. }
                        | FirConversionKind::NullabilityWidening { .. }
                )
            })
        }) else {
            return Ok(None);
        };
        let Some((destructure, statements, result)) = self.leading_destructure(container, body)
        else {
            return Ok(None);
        };
        let Some(components) = self.indexed_value_components(destructure_source, destructure)
        else {
            return Ok(None);
        };
        let element = self.allocate_local();
        let Some(nested) =
            self.with_index_nested_header(statement, receiver_source, receiver.value, element)?
        else {
            return Ok(None);
        };
        if matches!(nested, FirLoopHeader::Progression { .. })
            && !self.binds_element_as_counted(destructure, &components)
        {
            return Ok(None);
        }
        let index = self.allocate_local();
        if let Some(expression) = self.body.expr_mut(body) {
            expression.kind = FirExprKind::Block {
                statements: statements[1..].into(),
                result,
            };
        }
        Ok(Some((
            FirLoopHeader::WithIndex(Box::new(FirWithIndexLoop {
                nested,
                index,
                destructure,
                components,
            })),
            body,
        )))
    }

    /// The destructuring a checked loop body opens with, when it reads the loop's `container`,
    /// with the body's statements and result.
    fn leading_destructure(
        &self,
        container: LocalValueId,
        body: FirExprId,
    ) -> Option<(FirStatementId, Box<[FirStatementId]>, Option<FirExprId>)> {
        let FirExprKind::Block { statements, result } = &self.body.expr(body)?.kind else {
            return None;
        };
        let destructure = *statements.first()?;
        let FirStatementKind::Destructure { initializer, .. } =
            &self.body.statement(destructure)?.kind
        else {
            return None;
        };
        matches!(
            self.body.expr(*initializer)?.kind,
            FirExprKind::ValueRead(value) if value == container
        )
        .then(|| (destructure, statements.clone(), *result))
    }

    /// The `IndexedValue` component each entry of the destructuring `source` reads
    /// (`gatherLoopVariableInfo`): `componentN` by position, and the `index` and `value`
    /// properties by the selected declaration. An entry that reads anything else, or that reads a
    /// component only to discard it, keeps the loop iterating `IndexedValue`s.
    fn indexed_value_components(
        &self,
        source: StmtId,
        destructure: FirStatementId,
    ) -> Option<Box<[Option<FirIndexedValueComponent>]>> {
        let FirStatementKind::Destructure { entries, .. } = &self.body.statement(destructure)?.kind
        else {
            return None;
        };
        let properties = self.file.destructuring.source_properties.get(&source.0);
        entries
            .iter()
            .enumerate()
            .map(|(position, entry)| match entry {
                FirDestructureEntry::Ignored {
                    component: None, ..
                } => Some(None),
                FirDestructureEntry::Ignored { .. } => None,
                FirDestructureEntry::Binding { .. } => {
                    let by_property = properties
                        .and_then(|properties| properties.get(position))
                        .is_some_and(Option::is_some);
                    if by_property {
                        self.indexed_value_property(source, position).map(Some)
                    } else {
                        match position {
                            0 => Some(Some(FirIndexedValueComponent::Index)),
                            1 => Some(Some(FirIndexedValueComponent::Value)),
                            _ => None,
                        }
                    }
                }
            })
            .collect()
    }

    /// The `IndexedValue` property a name-based entry's selected read is
    /// (`STDLIB_INDEXED_VALUE_GET_INDEX_NAME`, `STDLIB_INDEXED_VALUE_GET_VALUE_NAME`).
    fn indexed_value_property(
        &self,
        source: StmtId,
        position: usize,
    ) -> Option<FirIndexedValueComponent> {
        let ResolvedCall::Member(selected) =
            &**self.info.resolved_destructure_component(source, position)?
        else {
            return None;
        };
        crate::trace_compiler!(
            "resolve",
            "withIndex component {position} reads {:?}.{}",
            selected.member.owner,
            selected.member.name,
        );
        if selected.member.owner != Some(crate::types::wk::indexed_value()) {
            return None;
        }
        match selected.member.name.as_str() {
            crate::types::wk::INDEXED_VALUE_INDEX => Some(FirIndexedValueComponent::Index),
            crate::types::wk::INDEXED_VALUE_VALUE => Some(FirIndexedValueComponent::Value),
            _ => None,
        }
    }

    /// A counted loop declares its element itself, as the plain value of its counter. An element
    /// binding the loop cannot declare that way (converted, or a `var`) keeps the loop iterating
    /// `IndexedValue`s.
    fn binds_element_as_counted(
        &self,
        destructure: FirStatementId,
        components: &[Option<FirIndexedValueComponent>],
    ) -> bool {
        let Some(FirStatementKind::Destructure { entries, .. }) = self
            .body
            .statement(destructure)
            .map(|statement| &statement.kind)
        else {
            return false;
        };
        entries
            .iter()
            .zip(components)
            .all(|(entry, component)| match entry {
                FirDestructureEntry::Binding {
                    mutable,
                    conversion,
                    ..
                } if *component == Some(FirIndexedValueComponent::Value) => {
                    !mutable && conversion.is_none()
                }
                _ => true,
            })
    }

    /// `NestedHeaderInfoBuilderForWithIndex`: the loop over the receiver of `withIndex()`, in the
    /// order of its handlers. A progression is counted, an array or a `String` is indexed, and an
    /// `Iterable` or a `Sequence` is iterated through the `iterator()` resolution selected for the
    /// receiver.
    fn with_index_nested_header(
        &mut self,
        statement: StmtId,
        receiver_source: ExprId,
        receiver: FirExprId,
        element: LocalValueId,
    ) -> Result<Option<FirLoopHeader>, BodyCheckFailure> {
        let span = self
            .file
            .stmt_spans
            .get(statement.0 as usize)
            .copied()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let receiver_ty = self
            .info
            .semantic_ty(receiver_source)
            .platform_lower_bound();
        let indexed = receiver_ty
            .array_read_elem()
            .map(|element| (element, FirBuiltinIterableKind::Array))
            .or((receiver_ty == Ty::String).then_some((Ty::Char, FirBuiltinIterableKind::String)));
        if let Some((element_ty, kind)) = indexed {
            return Ok(Some(FirLoopHeader::Iterable {
                variable: element,
                variable_ty: self.resolved_type(span, element_ty)?,
                kind,
                iterable: receiver,
            }));
        }
        let Some(protocol) = self.info.iterator_protocol(receiver_source).cloned() else {
            return Ok(None);
        };
        let element_ty = self.resolved_type(span, protocol.elem_ty)?;
        if let Some(header) =
            self.progression_loop_header(element, element_ty, receiver_source, receiver)?
        {
            return Ok(Some(header));
        }
        self.iterator_loop_header_from_protocol(statement, element, element_ty, receiver, &protocol)
            .map(Some)
    }
}
