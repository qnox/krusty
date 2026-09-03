//! Consuming checked-FIR to common-IR lowering.
//!
//! This layer receives final semantic decisions. It never accepts parser arenas, resolver state,
//! imports, or source spellings used for lookup.

mod arrays;
mod checked;
mod checked_arguments;
mod classifier_references;
mod constructors;
mod data_classes;
mod error;
mod expression;
mod external_references;
mod function_references;
mod generics;
mod initialization;
mod inlining;
mod interface_delegation;
mod local_callables;
mod loops;
mod module_declarations;
mod package_declarations;
mod properties;
mod property_references;
mod ranges;
mod sam_conversions;
mod sink;
mod source_calls;
mod statement;
mod suspend_conversions;
mod tailrec;
mod type_operations;

pub use error::*;
pub use sink::*;

use crate::fir::{BodyOwnerId, FirBody, FirExprId, FirStatementId, ResolvedModuleIndex};
use crate::ir::{ExprId, IrFile, IrNodeOrigin};
use std::collections::HashMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoweredFirBody {
    pub owner: BodyOwnerId,
    pub roots: Box<[ExprId]>,
    pub defaults: Box<[(u32, ExprId)]>,
    pub result_type: Option<crate::types::Ty>,
    pub implicit_return: bool,
    pub property_storage_type: Option<crate::types::Ty>,
    pub property_delegate: Option<crate::fir::FirPropertyDelegatePlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoweringState {
    Uncomputed,
    Computing,
    Lowered(ExprId),
}

#[cfg(feature = "trace")]
fn trace_checked_body(body: &FirBody, index: &ResolvedModuleIndex) {
    if !crate::trace::enabled("fir") {
        return;
    }
    let receiver_expressions = (0..body.expression_count())
        .filter_map(|raw| {
            let id = FirExprId::from_raw(u32::try_from(raw).ok()?);
            let expression = body.expr(id)?;
            matches!(
                expression.kind,
                crate::fir::FirExprKind::ImplicitReceiver { .. }
                    | crate::fir::FirExprKind::EnclosingReceiver { .. }
                    | crate::fir::FirExprKind::CapturedImplicitReceiver { .. }
                    | crate::fir::FirExprKind::ClassStorageRead { .. }
                    | crate::fir::FirExprKind::ConstructorCaptureRead { .. }
                    | crate::fir::FirExprKind::ConstructorContextRead { .. }
                    | crate::fir::FirExprKind::CapturedClassStorageRead { .. }
            )
            .then_some((id, expression.origin, expression.ty, &expression.kind))
        })
        .collect::<Vec<_>>();
    crate::trace_compiler!(
        "fir",
        "lower checked body owner={:?} declaration_name={:?} anchor={:?} local={:?} name={:?} receiver={:?} context={:?} context_values={} captures={:?} implicit_receiver_captures={:?} receiver_expressions={receiver_expressions:?}",
        body.owner(),
        index.declaration_name(crate::fir::DeclarationId::from_raw(body.owner().raw())),
        index.declaration_anchor(crate::fir::DeclarationId::from_raw(body.owner().raw())),
        body.local_callable(),
        body.debug_name(),
        body.receiver_type(),
        body.context_receiver_types(),
        body.context_value_count(),
        body.captures(),
        body.implicit_receiver_captures(),
    );
}

pub fn lower_body(
    body: FirBody,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<LoweredFirBody, FirLoweringFailure> {
    lower_body_with_context(
        body,
        index,
        ir,
        &mut LocalCallableLoweringContext::default(),
    )
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalCallableLoweringContext {
    realizations: HashMap<crate::fir::BodyLocalCallableDeclarationId, LocalCallableRealization>,
}

pub(crate) fn lower_body_with_context(
    body: FirBody,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    local_callables: &mut LocalCallableLoweringContext,
) -> Result<LoweredFirBody, FirLoweringFailure> {
    let owner = body.owner();
    #[cfg(feature = "trace")]
    trace_checked_body(&body, index);
    let declaration = crate::fir::DeclarationId::from_raw(owner.raw());
    let has_dispatch_receiver = index.enclosing_classifier(declaration).is_some();
    let mut lowering = BodyLowering::new(
        &body,
        index,
        ir,
        has_dispatch_receiver,
        HashMap::new(),
        vec![HashMap::new()],
        local_callables.realizations.clone(),
    );
    lowering.prepare_local_functions()?;
    lowering.realize_local_functions()?;
    let defaults = body
        .default_values()
        .iter()
        .map(|default| Ok((default.parameter, lowering.expression(default.value)?)))
        .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
    let mut roots = Vec::new();
    for root in body.roots().iter().copied() {
        let lowered = lowering.statement(root)?;
        // A destructuring declaration lowers to several declarations in the surrounding lexical
        // scope. `IrExpr::Block` deliberately scopes its locals, so retaining the wrapper would
        // make the component locals disappear before the following source statement. Flatten only
        // this checked statement form at the callable-root boundary; expression blocks and control
        // flow blocks keep their ordinary lexical scope.
        if matches!(
            body.statement(root).map(|statement| &statement.kind),
            Some(crate::fir::FirStatementKind::Destructure { .. })
        ) {
            let crate::ir::IrExpr::Block { stmts, value: None } = lowering.ir.expr(lowered) else {
                return Err(FirLoweringFailure::MalformedDestructureLowering {
                    origin: body
                        .statement(root)
                        .expect("a FIR root must be a statement")
                        .origin,
                });
            };
            roots.extend(stmts.iter().copied());
        } else {
            roots.push(lowered);
        }
    }
    let lowered = LoweredFirBody {
        owner,
        roots: roots.into_boxed_slice(),
        defaults: defaults.into_boxed_slice(),
        result_type: body.result_type().map(crate::fir::ResolvedTy::get),
        implicit_return: body.has_implicit_return(),
        property_storage_type: body
            .property_storage_type()
            .map(crate::fir::ResolvedTy::get),
        property_delegate: body.property_delegate().cloned(),
    };
    local_callables.realizations = lowering.published_local_callables;
    Ok(lowered)
}

struct BodyLowering<'a> {
    body: &'a FirBody,
    index: &'a ResolvedModuleIndex,
    ir: &'a mut IrFile,
    expression_states: Vec<LoweringState>,
    statement_states: Vec<LoweringState>,
    next_temporary: u32,
    has_dispatch_receiver: bool,
    context_parameter_count: u32,
    context_value_count: u32,
    has_extension_receiver: bool,
    capture_count: u32,
    class_constructor_capture_count: u32,
    class_constructor_context_count: u32,
    capture_slots: HashMap<(u32, crate::fir::LocalValueId), CaptureSlot>,
    implicit_receiver_capture_slots: Vec<(crate::fir::FirImplicitReceiverCapture, u32)>,
    shared_locals: HashMap<crate::fir::LocalValueId, crate::fir::ResolvedTy>,
    local_class_captures: HashMap<crate::types::TypeName, Vec<(ExprId, crate::types::Ty)>>,
    local_callable_scopes: Vec<HashMap<crate::fir::LocalCallableId, LocalCallableRealization>>,
    published_local_callables:
        HashMap<crate::fir::BodyLocalCallableDeclarationId, LocalCallableRealization>,
    control_path: Vec<crate::fir::LocalCallableId>,
}

#[derive(Clone, Debug)]
pub(crate) struct LocalCallableRealization {
    function: crate::ir::FunId,
    owner: Option<crate::types::TypeName>,
    source_name: Box<str>,
    captures: Vec<crate::fir::FirCapture>,
    implicit_receiver_captures: Vec<crate::fir::FirImplicitReceiverCapture>,
    context_parameter_count: u32,
    has_extension_receiver: bool,
}

impl LocalCallableRealization {
    fn capture_count(&self) -> usize {
        self.captures.len() + self.implicit_receiver_captures.len()
    }
}

#[derive(Clone, Copy, Debug)]
struct CaptureSlot {
    slot: u32,
    ty: crate::fir::ResolvedTy,
    shared_cell: bool,
}

impl<'a> BodyLowering<'a> {
    fn new(
        body: &'a FirBody,
        index: &'a ResolvedModuleIndex,
        ir: &'a mut IrFile,
        has_dispatch_receiver: bool,
        capture_slots: HashMap<(u32, crate::fir::LocalValueId), CaptureSlot>,
        local_callable_scopes: Vec<HashMap<crate::fir::LocalCallableId, LocalCallableRealization>>,
        published_local_callables: HashMap<
            crate::fir::BodyLocalCallableDeclarationId,
            LocalCallableRealization,
        >,
    ) -> Self {
        let context_parameter_count = u32::try_from(body.context_receiver_types().len())
            .expect("too many FIR context parameters");
        let context_value_count = body.context_value_count();
        let unbound_context_count = context_parameter_count
            .checked_sub(context_value_count)
            .expect("FIR context values exceed context receivers");
        let has_extension_receiver = body.receiver_type().is_some();
        let capture_count =
            u32::try_from(body.captures().len() + body.implicit_receiver_captures().len())
                .expect("too many FIR captures");
        let implicit_receiver_capture_slots = body
            .implicit_receiver_captures()
            .iter()
            .enumerate()
            .map(|(ordinal, capture)| {
                (
                    capture.clone(),
                    u32::try_from(body.captures().len() + ordinal).expect("too many FIR captures"),
                )
            })
            .collect();
        let class_constructor_capture_count = body.constructor_capture_parameter_count();
        let class_constructor_context_count = body.constructor_context_parameter_count();
        let value_slot_count = body
            .local_value_count()
            .checked_add(capture_count)
            .and_then(|count| count.checked_add(class_constructor_capture_count))
            .and_then(|count| count.checked_add(class_constructor_context_count))
            .and_then(|count| count.checked_add(u32::from(has_dispatch_receiver)))
            .and_then(|count| count.checked_add(unbound_context_count))
            .and_then(|count| count.checked_add(u32::from(has_extension_receiver)))
            .expect("too many FIR value slots");
        Self {
            expression_states: vec![LoweringState::Uncomputed; body.expression_count()],
            statement_states: vec![LoweringState::Uncomputed; body.statement_count()],
            body,
            index,
            ir,
            next_temporary: value_slot_count,
            has_dispatch_receiver,
            context_parameter_count,
            context_value_count,
            has_extension_receiver,
            capture_count,
            class_constructor_capture_count,
            class_constructor_context_count,
            capture_slots,
            implicit_receiver_capture_slots,
            shared_locals: directly_shared_locals(body),
            local_class_captures: HashMap::new(),
            local_callable_scopes,
            published_local_callables,
            control_path: Vec::new(),
        }
    }

    fn control_label(
        &self,
        target_depth: u32,
        target: crate::fir::ControlTargetId,
    ) -> Result<String, FirLoweringFailure> {
        let target_depth = usize::try_from(target_depth).map_err(|_| {
            FirLoweringFailure::MissingControlTarget {
                target,
                target_depth,
            }
        })?;
        let path_len = self.control_path.len();
        let keep =
            path_len
                .checked_sub(target_depth)
                .ok_or(FirLoweringFailure::MissingControlTarget {
                    target,
                    target_depth: u32::try_from(target_depth).unwrap_or(u32::MAX),
                })?;
        let mut label = format!("$fir_control_{}", self.body.owner().raw());
        for callable in &self.control_path[..keep] {
            label.push('_');
            label.push_str(&callable.raw().to_string());
        }
        label.push('_');
        label.push_str(&target.raw().to_string());
        Ok(label)
    }

    fn shared_local_type(&self, value: crate::fir::LocalValueId) -> Option<crate::fir::ResolvedTy> {
        self.shared_locals.get(&value).copied()
    }

    fn expression_state(&self, expression: FirExprId) -> Option<LoweringState> {
        self.expression_states
            .get(expression.raw() as usize)
            .copied()
    }

    fn set_expression_state(&mut self, expression: FirExprId, state: LoweringState) {
        self.expression_states[expression.raw() as usize] = state;
    }

    fn statement_state(&self, statement: FirStatementId) -> Option<LoweringState> {
        self.statement_states.get(statement.raw() as usize).copied()
    }

    fn set_statement_state(&mut self, statement: FirStatementId, state: LoweringState) {
        self.statement_states[statement.raw() as usize] = state;
    }

    fn allocate_temporary(&mut self) -> u32 {
        let temporary = self.next_temporary;
        self.next_temporary = self
            .next_temporary
            .checked_add(1)
            .expect("too many FIR lowering temporaries");
        temporary
    }

    /// Build the implementation body of a function-like reference after the frontend has selected
    /// its target and adaptation. Kotlin `Unit` is `void` only at a declared-call boundary; a
    /// `FunctionN.invoke` result is a value and therefore returns the `kotlin.Unit` singleton after
    /// discarding the selected callable's result.
    fn callable_reference_adapter_body(
        &mut self,
        value: ExprId,
        selected_result: crate::types::Ty,
        reference_result: crate::types::Ty,
    ) -> ExprId {
        let mut statements = Vec::with_capacity(2);
        let result = if reference_result == crate::types::Ty::Unit {
            statements.push(value);
            self.ir.add_expr(crate::ir::IrExpr::UnitInstance)
        } else if selected_result == reference_result {
            value
        } else {
            self.ir.add_expr(crate::ir::IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: reference_result,
            })
        };
        statements.push(self.ir.add_expr(crate::ir::IrExpr::Return(Some(result))));
        self.ir.add_expr(crate::ir::IrExpr::Block {
            stmts: statements,
            value: None,
        })
    }

    fn value_slot(&self, value: crate::fir::LocalValueId) -> u32 {
        let mut slot = self.capture_count
            + self.class_constructor_capture_count
            + self.class_constructor_context_count
            + value.raw()
            + u32::from(self.has_dispatch_receiver);
        if value.raw() >= self.context_value_count {
            slot += self.context_parameter_count - self.context_value_count;
            slot += u32::from(self.has_extension_receiver);
        }
        slot
    }

    fn implicit_receiver_slot(&self, current: bool, depth: u32) -> Option<u32> {
        let context_start = self.capture_count
            + u32::from(self.has_dispatch_receiver)
            + self.class_constructor_capture_count
            + self.class_constructor_context_count;
        let extension = self
            .has_extension_receiver
            .then_some(context_start + self.context_parameter_count);
        let dispatch = self.has_dispatch_receiver.then_some(self.capture_count);
        let receivers = extension
            .into_iter()
            .chain(
                (0..self.context_parameter_count)
                    .rev()
                    .map(|ordinal| context_start + ordinal),
            )
            .chain(dispatch);
        if current {
            receivers.into_iter().next()
        } else {
            receivers.into_iter().nth(depth as usize)
        }
    }

    fn implicit_receiver_capture_slot(
        &self,
        enclosing_depth: u32,
        current: bool,
        depth: u32,
        path: &[crate::fir::DeclarationId],
    ) -> Option<u32> {
        self.implicit_receiver_capture_slots
            .iter()
            .find_map(|(capture, slot)| {
                (capture.enclosing_depth == enclosing_depth
                    && capture.current == current
                    && capture.depth == depth
                    && capture.path.as_ref() == path)
                    .then_some(*slot)
            })
    }

    fn dispatch_receiver_slot(&self) -> Option<u32> {
        self.has_dispatch_receiver.then_some(self.capture_count)
    }

    fn record_expression_origins(
        &mut self,
        first: usize,
        root: ExprId,
        cause: crate::fir::OriginId,
    ) {
        for raw in first..self.ir.exprs.len() {
            let expression = u32::try_from(raw).expect("too many common IR expressions");
            self.ir
                .fir_origins
                .entry(expression)
                .or_insert(IrNodeOrigin::Synthetic {
                    cause,
                    kind: crate::fir::SyntheticOriginKind::GeneratedControlFlow,
                });
        }
        self.ir.fir_origins.insert(root, IrNodeOrigin::Fir(cause));
    }
}

fn directly_shared_locals(
    body: &FirBody,
) -> HashMap<crate::fir::LocalValueId, crate::fir::ResolvedTy> {
    let mut shared = HashMap::new();
    let add = |shared: &mut HashMap<_, _>, nested: &FirBody| {
        for capture in nested
            .captures()
            .iter()
            .filter(|capture| capture.enclosing_depth == 0 && capture.shared_cell)
        {
            shared.insert(capture.source, capture.ty);
        }
    };
    let add_class = |shared: &mut HashMap<_, _>, captures: &[crate::fir::FirLocalClassCapture]| {
        for capture in captures.iter().filter(|capture| capture.shared_cell) {
            if let crate::fir::FirLocalClassCaptureSource::Value(source) = &capture.source {
                shared.insert(*source, capture.ty);
            }
        }
    };
    for raw in 0..body.statement_count() {
        let statement = crate::fir::FirStatementId::from_raw(
            u32::try_from(raw).expect("too many FIR statements"),
        );
        if let Some(crate::fir::FirStatementKind::LocalFunction { body, .. }) =
            body.statement(statement).map(|statement| &statement.kind)
        {
            add(&mut shared, body);
        }
        if let Some(crate::fir::FirStatementKind::LocalDeclaration { captures, .. }) =
            body.statement(statement).map(|statement| &statement.kind)
        {
            add_class(&mut shared, captures);
        }
    }
    for raw in 0..body.expression_count() {
        let expression =
            crate::fir::FirExprId::from_raw(u32::try_from(raw).expect("too many FIR expressions"));
        if let Some(crate::fir::FirExprKind::Lambda { body, .. }) =
            body.expr(expression).map(|expression| &expression.kind)
        {
            add(&mut shared, body);
        }
        if let Some(crate::fir::FirExprKind::AnonymousObject(object)) =
            body.expr(expression).map(|expression| &expression.kind)
        {
            add_class(&mut shared, &object.captures);
        }
    }
    shared
}

fn finish_callable_body(
    ir: &mut IrFile,
    mut roots: Vec<ExprId>,
    result: crate::types::Ty,
    implicit_return: bool,
    unit_as_value: bool,
    origin: crate::fir::OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    let first_generated = ir.exprs.len();
    let body = if !implicit_return {
        ir.add_expr(crate::ir::IrExpr::Block {
            stmts: roots,
            value: None,
        })
    } else if result == crate::types::Ty::Unit {
        let return_unit = if unit_as_value {
            let unit = ir.add_expr(crate::ir::IrExpr::UnitInstance);
            ir.add_expr(crate::ir::IrExpr::Return(Some(unit)))
        } else {
            ir.add_expr(crate::ir::IrExpr::Return(None))
        };
        roots.push(return_unit);
        ir.add_expr(crate::ir::IrExpr::Block {
            stmts: roots,
            value: None,
        })
    } else {
        let value = roots
            .pop()
            .ok_or(FirLoweringFailure::MissingBodyResult { origin })?;
        let return_value = ir.add_expr(crate::ir::IrExpr::Return(Some(value)));
        roots.push(return_value);
        ir.add_expr(crate::ir::IrExpr::Block {
            stmts: roots,
            value: None,
        })
    };
    for raw in first_generated..ir.exprs.len() {
        let expression = u32::try_from(raw).expect("too many common IR expressions");
        ir.fir_origins.insert(
            expression,
            IrNodeOrigin::Synthetic {
                cause: origin,
                kind: crate::fir::SyntheticOriginKind::GeneratedControlFlow,
            },
        );
    }
    Ok(body)
}

#[cfg(test)]
mod tests;
