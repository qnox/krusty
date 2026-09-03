//! Transient AST to checked FIR construction.
//!
//! This module owns source-shape traversal only. Semantic types and selections come from the
//! ordinary checker; unsupported migration seams are explicit errors rather than placeholder FIR.

mod arguments;
mod arrays;
#[cfg(test)]
mod assignment_tests;
mod assignments;
#[cfg(test)]
mod call_tests;
mod calls;
#[cfg(test)]
mod collection_literal_tests;
#[cfg(test)]
mod constructor_tests;
mod constructors;
#[cfg(test)]
mod control_flow_tests;
#[cfg(test)]
mod delegate_tests;
mod delegates;
mod destructure;
#[cfg(test)]
mod destructure_tests;
mod driver;
#[cfg(test)]
mod driver_tests;
#[cfg(test)]
mod invoke_tests;
mod invokes;
#[cfg(test)]
mod iterator_tests;
mod iterators;
#[cfg(test)]
mod lambda_tests;
mod lambdas;
#[cfg(test)]
mod local_class_tests;
mod local_classes;
#[cfg(test)]
mod local_function_tests;
mod local_functions;
mod plugins;
mod properties;
#[cfg(test)]
mod property_tests;
#[cfg(test)]
mod receiver_tests;
#[cfg(test)]
mod reference_tests;
mod references;
#[cfg(test)]
mod test_support;
mod type_materialization;
#[cfg(test)]
mod type_operation_tests;

pub(crate) use driver::check_and_dispatch_active_body_in_session;
pub(crate) use driver::check_and_dispatch_signature_defaults_in_session;
pub use driver::{
    check_and_dispatch_body, check_and_dispatch_body_in_session,
    check_and_dispatch_scheduled_function_body,
    check_and_dispatch_scheduled_function_body_in_session,
};

use std::collections::HashMap;

use crate::ast::{BinOp, Expr, ExprId, File, RangeKind, Stmt, StmtId, TemplatePart, UnOp};
use crate::diag::Span;
use crate::resolve::{
    ExprLowering, IncDecSite, InvokeKind, ResolvedCall, ReturnTarget, StmtLowering, TypeInfo,
};
use crate::types::Ty;

use super::coverage::{ExpressionForm, StatementForm};
use super::{
    dispatch_checked_body, ActiveSourceDeclarations, BodyKind, BodyLocalCallableDeclarationId,
    BodyOwnerId, BodyWorkItem, CheckedBodySink, ClassBodyContext, ClassCaptureBinding,
    ClassReceiverCaptureSource, ControlTargetId, DeclarationId, DeclarationKind,
    DefaultArgumentProvider, DelegateStorage, ExternalCallableId, FirAdaptedReferenceArgument,
    FirAnonymousObject, FirArrayElement, FirBinaryOperation, FirBody, FirBuiltinIterableKind,
    FirCall, FirCallArgument, FirCallTarget, FirCallableReferenceBinding,
    FirCallableReferenceTarget, FirCapture, FirCatch, FirClassifierProperty, FirConstant,
    FirConstructorCall, FirConstructorTarget, FirControlTarget, FirControlTargetKind,
    FirConversion, FirConversionKind, FirDefaultValue, FirDelegateCall,
    FirDelegateDispatchReceiver, FirDestructureEntry, FirExpr, FirExprId, FirExprKind,
    FirImplicitReceiverCapture, FirIndexedAccessKind, FirInterfaceDelegateArgument, FirIntrinsic,
    FirJumpKind, FirLocalCallableRef, FirLocalClassCapture, FirLocalClassCaptureSource,
    FirLoopHeader, FirPluginOperand, FirPropertyDelegatePlan, FirPropertyReferenceTarget,
    FirPropertyTarget, FirRangeOperation, FirReceiver, FirReferenceAdaptation, FirSamConversion,
    FirStatement, FirStatementId, FirStatementKind, FirTypeOperation, FirTypeParameterRef,
    FirTypeSubstitution, FirUnaryOperation, FirValueParameter, FirVarargElement, FirWhenBranch,
    FirWhenCondition, InlineBodyStore, LocalBinding, LocalCallableId, LocalDelegateBinding,
    LocalValueId, OriginId, OriginStore, PropertyId, ResolvedCallableHeader, ResolvedModuleIndex,
    ResolvedTy, SourceFileId, SyntheticOriginKind, UnpublishableType,
};

fn checked_constant_value(constant: &crate::libraries::LibraryConst) -> FirConstant {
    match &constant.value {
        crate::libraries::LibConst::Int(value) => match constant.ty.non_null() {
            Ty::Boolean => FirConstant::Boolean(*value != 0),
            Ty::Char => FirConstant::Char(*value as u16),
            Ty::UInt | Ty::UByte | Ty::UShort => FirConstant::UInt(i64::from(*value as u32)),
            _ => FirConstant::Int(i64::from(*value)),
        },
        crate::libraries::LibConst::Long(value) => match constant.ty.non_null() {
            Ty::ULong => FirConstant::ULong(*value),
            _ => FirConstant::Long(*value),
        },
        crate::libraries::LibConst::Float(value) => FirConstant::Float(*value),
        crate::libraries::LibConst::Double(value) => FirConstant::Double(*value),
        crate::libraries::LibConst::Str(value) => FirConstant::String(value.clone()),
    }
}

/// Bind a parser-local statement to its declaration-stream identity without retaining either the
/// parser id or a text coordinate. Parsing the same bounded declaration unit produces the same
/// local-function stream; the identity is discarded with the active checker after use.
fn body_local_callable_declaration(
    file: &File,
    index: &ResolvedModuleIndex,
    owner: BodyOwnerId,
    statement: StmtId,
) -> Option<BodyLocalCallableDeclarationId> {
    let mut declaration = DeclarationId::from_raw(owner.raw());
    while let Some(parent) = index
        .declaration_anchor(declaration)
        .and_then(|anchor| anchor.owner)
    {
        declaration = parent;
    }
    let owner = BodyOwnerId::from_raw(declaration.raw());
    let mut ordinal = 0u32;
    for (raw, candidate) in file.stmt_arena.iter().enumerate() {
        if !matches!(candidate, Stmt::LocalFun(_)) {
            continue;
        }
        if raw == statement.0 as usize {
            return Some(BodyLocalCallableDeclarationId::new(owner, ordinal));
        }
        ordinal = ordinal.checked_add(1).expect("too many local functions");
    }
    None
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BodyCheckFailureKind {
    MissingSourceSpan,
    UnpublishableType(UnpublishableType),
    UnresolvedTypeSyntax,
    UnknownLocal,
    InvalidAnnotationArgument,
    MissingStableCallTarget,
    MissingStablePropertyTarget,
    LocalVariableCallableReference,
    UnsupportedCallShape,
    UnsupportedExpression(ExpressionForm),
    UnsupportedStatement(StatementForm),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyCheckFailure {
    pub span: Option<Span>,
    pub kind: BodyCheckFailureKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckedBodyParameter<'a> {
    pub name: &'a str,
    pub ty: ResolvedTy,
    pub span: Span,
}

#[derive(Clone, Copy)]
struct CheckedBodyReceiverShape<'a> {
    context_receivers: &'a [ResolvedTy],
    context_value_count: u32,
    extension_receiver: Option<ResolvedTy>,
}

impl CheckedBodyReceiverShape<'_> {
    const EMPTY: Self = Self {
        context_receivers: &[],
        context_value_count: 0,
        extension_receiver: None,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CheckedBodyDefault {
    parameter: u32,
    expression: ExprId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckedBodyDriverFailure {
    SourceMismatch,
    MissingCallable,
    MissingBody,
    BodyRangeMismatch,
    ParameterShapeMismatch,
    UnsupportedBodyKind(BodyKind),
    Check(BodyCheckFailure),
}

/// Check and route one scheduled expression body. A declaration/body-range parser supplies the
/// transient `File` and root expression; this function validates the stable work identity, creates
/// checked FIR, and moves it directly across the inline-or-consuming sink boundary.
#[allow(clippy::too_many_arguments)]
pub fn check_and_dispatch_expression_body(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    root: ExprId,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
) -> Result<(), CheckedBodyDriverFailure> {
    if index
        .declaration_anchor(work.declaration)
        .is_none_or(|anchor| anchor.source != source)
    {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let callable = index
        .callable_for_declaration(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let body = check_expression_body(file, info, source, work.owner, root, index, origins)
        .map_err(CheckedBodyDriverFailure::Check)?;
    dispatch_checked_body(callable, work, body, inline_bodies, ordinary_sink);
    Ok(())
}

/// Build one checked FIR body from a transient, already-checked AST expression. The result owns no
/// parser ids or source spellings. Callers consume it immediately or retain it only for inline code.
pub fn check_expression_body(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    owner: BodyOwnerId,
    root: ExprId,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
) -> Result<FirBody, BodyCheckFailure> {
    let mut session = BodyCheckSession::default();
    check_expression_body_with_parameters_in_session(
        file,
        info,
        source,
        owner,
        root,
        &[],
        index,
        origins,
        &mut session,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn check_expression_body_with_parameters(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    owner: BodyOwnerId,
    root: ExprId,
    parameters: &[CheckedBodyParameter<'_>],
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
) -> Result<FirBody, BodyCheckFailure> {
    let mut session = BodyCheckSession::default();
    check_expression_body_with_parameters_in_session(
        file,
        info,
        source,
        owner,
        root,
        parameters,
        index,
        origins,
        &mut session,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_expression_body_with_parameters_in_session(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    owner: BodyOwnerId,
    root: ExprId,
    parameters: &[CheckedBodyParameter<'_>],
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    session: &mut BodyCheckSession,
) -> Result<FirBody, BodyCheckFailure> {
    check_body_unit_with_parameters_and_defaults(
        file,
        info,
        source,
        owner,
        file.expr_span(root).ok_or(BodyCheckFailure {
            span: None,
            kind: BodyCheckFailureKind::MissingSourceSpan,
        })?,
        Some(root),
        parameters,
        &[],
        CheckedBodyReceiverShape::EMPTY,
        None,
        index,
        origins,
        session,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_body_unit_with_parameters_and_defaults(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    owner: BodyOwnerId,
    root_span: Span,
    root: Option<ExprId>,
    parameters: &[CheckedBodyParameter<'_>],
    defaults: &[CheckedBodyDefault],
    receiver_shape: CheckedBodyReceiverShape<'_>,
    property_storage_type: Option<ResolvedTy>,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    session: &mut BodyCheckSession,
) -> Result<FirBody, BodyCheckFailure> {
    if root.is_none() && defaults.is_empty() {
        return Err(BodyCheckFailure {
            span: Some(root_span),
            kind: BodyCheckFailureKind::MissingSourceSpan,
        });
    }
    let mut checker = BodyFirChecker::new(
        file, info, source, owner, root_span, index, origins, session,
    );
    if let Some(storage) = property_storage_type {
        checker
            .body
            .replace_result_type_with_property_storage(storage);
    }
    checker.configure_receivers(
        receiver_shape.context_receivers,
        receiver_shape.context_value_count,
        receiver_shape.extension_receiver,
    );
    bind_parameters_and_check_defaults(&mut checker, parameters, defaults, receiver_shape)?;
    if let Some(root) = root {
        let origin = checker.expression_origin(root)?;
        let value = checker.expression(root)?;
        // Expression-bodied declarations and checked property initializers are value boundaries.
        // Their finalized signature result is installed by `BodyFirChecker::new`; apply only a
        // conversion the resolver explicitly selected for this root. Block bodies have no such
        // root decision and therefore remain untouched.
        let value_type = checker
            .body
            .expr(value)
            .map(|expression| expression.ty)
            .ok_or_else(|| checker.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        let recorded_root_conversion = checker
            .info
            .selected_numeric_conversions
            .get(&root)
            .is_some_and(|selected| ResolvedTy::new(*selected).ok() == checker.body.result_type())
            || checker.info.resolved_sam_conversions.contains_key(&root)
            || checker
                .info
                .selected_suspend_function_conversions
                .get(&root)
                .is_some_and(|(_, selected)| {
                    ResolvedTy::new(*selected).ok() == checker.body.result_type()
                })
            || checker
                .info
                .selected_value_smartcasts
                .get(&root)
                .is_some_and(|selected| {
                    ResolvedTy::new(*selected).ok() == checker.body.result_type()
                });
        let expression = match checker
            .body
            .result_type()
            .filter(|target| *target != value_type && recorded_root_conversion)
        {
            Some(target) => match checker.selected_value_conversion(root, target, origin)? {
                Some(conversion) => checker.body.add_expr(FirExpr {
                    origin,
                    ty: target,
                    kind: FirExprKind::ImplicitConversion { value, conversion },
                }),
                None => value,
            },
            None => value,
        };
        let statement = checker.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(expression),
        });
        checker.body.push_root(statement);
    }
    checker.body.finalize_capture_forwarding();
    Ok(checker.body)
}

fn bind_parameters_and_check_defaults(
    checker: &mut BodyFirChecker<'_>,
    parameters: &[CheckedBodyParameter<'_>],
    defaults: &[CheckedBodyDefault],
    receiver_shape: CheckedBodyReceiverShape<'_>,
) -> Result<(), BodyCheckFailure> {
    let context_parameter_count = u32::try_from(receiver_shape.context_receivers.len())
        .map_err(|_| checker.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
    if receiver_shape.context_value_count > context_parameter_count {
        return Err(checker.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
    }
    let mut defaults = defaults.iter().peekable();
    for (ordinal, parameter) in parameters.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| {
            checker.failure(
                Some(parameter.span),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        while defaults
            .peek()
            .is_some_and(|default| default.parameter == ordinal)
        {
            let default = defaults.next().expect("peeked constructor default");
            let value = checker.expression(default.expression)?;
            let origin = checker.expression_origin(default.expression)?;
            checker.body.add_default_value(FirDefaultValue {
                origin,
                parameter: default.parameter,
                value,
            });
        }
        if ordinal >= receiver_shape.context_value_count && ordinal < context_parameter_count {
            continue;
        }
        let value = if parameter.name == "_" {
            checker.allocate_local()
        } else {
            checker.bind_local(parameter.name, parameter.ty)
        };
        let origin = checker.origins.source(checker.source, parameter.span);
        checker.body.add_parameter(FirValueParameter {
            origin,
            value,
            ty: parameter.ty,
        });
    }
    if defaults.next().is_some() {
        return Err(checker.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
    }
    Ok(())
}

struct BodyFirChecker<'a> {
    file: &'a File,
    info: &'a TypeInfo,
    source: SourceFileId,
    index: &'a ResolvedModuleIndex,
    origins: &'a mut OriginStore,
    body: FirBody,
    scopes: Vec<HashMap<String, LocalBinding>>,
    delegate_scopes: Vec<HashMap<String, LocalDelegateBinding>>,
    outer_values: HashMap<String, (u32, LocalBinding)>,
    outer_delegates: HashMap<String, (u32, LocalDelegateBinding)>,
    class_values: HashMap<String, ClassCaptureBinding>,
    class_delegates: HashMap<String, LocalDelegateBinding>,
    class_receivers: Vec<ClassCaptureBinding>,
    /// Stable property whose accessor lexically contains this body, propagated through local-class
    /// body units by [`BodyCheckSession`]. This replaces an impossible parent walk: local classifier
    /// anchors are module identities and deliberately do not retain parser-body ownership.
    enclosing_property: Option<PropertyId>,
    session: &'a mut BodyCheckSession,
    return_target: ControlTargetId,
    /// The lambda/anonymous-function expression whose body this checker is constructing, when it is
    /// not the declaration's own body. A `return` inside it is recorded as
    /// [`ReturnTarget::Lambda`] of exactly this expression — a bare `return` in an anonymous
    /// function, or a `return@label` in a lambda — and resolves to this checker's own control
    /// target rather than the enclosing declaration's.
    lambda_return_source: Option<ExprId>,
    /// Enclosing lambda expression identities and their lexical body distance. This is transient
    /// checker state only; checked FIR publishes the numeric distance, never an AST id.
    outer_lambda_return_depths: HashMap<ExprId, u32>,
    /// Distance to the nearest named/anonymous function body. A bare return inside nested inline
    /// lambdas targets that function; entering a local function resets the distance to zero.
    function_return_depth: u32,
    loops: Vec<(Option<String>, u32, ControlTargetId)>,
    local_callable_scopes: Vec<HashMap<StmtId, LocalCallableId>>,
    /// Temporary AST-expression replacements used while publishing evaluation-order-sensitive
    /// desugarings. The map is scoped to one statement and never survives checked FIR.
    expression_substitutions: HashMap<ExprId, FirExprId>,
    outer_callables: HashMap<BodyLocalCallableDeclarationId, (u32, LocalCallableId)>,
    /// Callables whose declaration lives in an enclosing independently streamed body. Nested
    /// lambdas/local functions retain this set so their references publish explicit closure
    /// operands instead of mistaking a streaming boundary for an ordinary nested-body scope.
    streamed_outer_callables: std::collections::HashSet<BodyLocalCallableDeclarationId>,
    nested_body_depth: u32,
    owned_receiver_count: u32,
    outer_receiver_frames: Vec<ReceiverFrame>,
    /// Constructor delegation and constructor-owned defaults execute before local-class capture
    /// fields are readable from `this`; capture accesses in those regions use prefix parameters.
    constructor_prefix_capture_access: bool,
}

#[derive(Clone, Debug)]
struct ReceiverFrame {
    /// Width in resolver receiver-tower coordinates, including named context values that are
    /// materialized as ordinary FIR parameters rather than receiver slots.
    width: u32,
    /// Stable owner of this frame's dispatch receiver. Enum entries are classifier-like semantic
    /// owners even though their anonymous runtime subclass is not a source classifier header.
    dispatch_owner: Option<DeclarationId>,
    /// Runtime receiver-slot coordinate of `dispatch_owner` inside this frame.
    dispatch_depth: Option<u32>,
    /// Resolver coordinate to runtime receiver-slot coordinate for capturable receivers. Named
    /// context values are absent because their stable `context_binding` captures the value.
    capture_depths: HashMap<u32, u32>,
    /// Receiver coordinates in this frame that are reached through checked enclosing-instance
    /// edges rather than direct callable slots.
    structural_paths: HashMap<u32, Box<[DeclarationId]>>,
}

/// Transient checked context shared only by body callbacks for the currently active source unit.
/// Any Pass-1 context needed later is owned by its retained inline/default [`FirBody`] and copied
/// into a fresh session at the start of Pass 2; the session itself never crosses the pass boundary.
#[derive(Default)]
pub struct BodyCheckSession {
    class_bodies: HashMap<DeclarationId, ClassBodyContext>,
    local_callables: HashMap<BodyLocalCallableDeclarationId, PublishedLocalCallable>,
    active_source: Option<ActiveSourceDeclarations>,
}

#[derive(Clone, Debug)]
struct PublishedLocalCallable {
    captures: Box<[FirCapture]>,
    implicit_receiver_captures: Box<[FirImplicitReceiverCapture]>,
}

impl BodyCheckSession {
    fn install_active_source(&mut self, active: &ActiveSourceDeclarations) {
        // Pass 2 binds a fresh parser arena for every top-level declaration unit. Two successive
        // bindings can belong to the same source file while assigning the same transient DeclId to
        // different stable local classifiers, so source identity alone cannot make this cacheable.
        self.active_source = Some(active.clone());
    }

    pub(crate) fn absorb_retained_body(&mut self, body: &FirBody) {
        body.collect_class_body_contexts(&mut self.class_bodies);
        self.absorb_checked_body(body);
    }

    fn absorb_checked_body(&mut self, body: &FirBody) {
        for raw in 0..body.statement_count() {
            let statement = FirStatementId::from_raw(
                u32::try_from(raw).expect("too many FIR statements for a packed identity"),
            );
            let Some(FirStatement {
                kind:
                    FirStatementKind::LocalFunction {
                        declaration, body, ..
                    },
                ..
            }) = body.statement(statement)
            else {
                continue;
            };
            let published = PublishedLocalCallable {
                captures: body.captures().to_vec().into_boxed_slice(),
                implicit_receiver_captures: body
                    .implicit_receiver_captures()
                    .to_vec()
                    .into_boxed_slice(),
            };
            if let Some(previous) = self.local_callables.insert(*declaration, published.clone()) {
                debug_assert_eq!(previous.captures, published.captures);
                debug_assert_eq!(
                    previous.implicit_receiver_captures,
                    published.implicit_receiver_captures
                );
            }
            self.absorb_checked_body(body);
        }
    }
}

impl BodyFirChecker<'_> {
    fn safe_selector_receiver(kind: &FirExprKind) -> Option<FirReceiver> {
        match kind {
            FirExprKind::Call(call) => call.extension_receiver.or(call.dispatch_receiver),
            FirExprKind::LocalCall {
                extension_receiver: Some(receiver),
                ..
            } => Some(*receiver),
            FirExprKind::PropertyRead {
                dispatch_receiver,
                extension_receiver,
                ..
            }
            | FirExprKind::PropertyWrite {
                dispatch_receiver,
                extension_receiver,
                ..
            } => extension_receiver.or(*dispatch_receiver),
            // A provider-marked primitive member (`a?.plus(b)`) is already a checked binary
            // operation. Its left operand is still the exact selected dispatch receiver, so retain
            // that ownership for the safe-call null guard instead of reconstructing a callable.
            FirExprKind::Binary { lhs, .. } | FirExprKind::Range { start: lhs, .. } => {
                Some(FirReceiver {
                    value: *lhs,
                    conversion: None,
                })
            }
            FirExprKind::Unary { operand, .. }
            | FirExprKind::ImplicitConversion { value: operand, .. } => Some(FirReceiver {
                value: *operand,
                conversion: None,
            }),
            FirExprKind::FunctionInvoke { callee, .. } => Some(FirReceiver {
                value: *callee,
                conversion: None,
            }),
            FirExprKind::ConstructorCall(call) => call.outer_receiver,
            FirExprKind::Constant(_)
            | FirExprKind::AnnotationArray(_)
            | FirExprKind::ArrayLiteral { .. }
            | FirExprKind::ArrayConstruction { .. }
            | FirExprKind::ImplicitReceiver { .. }
            | FirExprKind::EnclosingReceiver { .. }
            | FirExprKind::CapturedImplicitReceiver { .. }
            | FirExprKind::SingletonValue { .. }
            | FirExprKind::EnumEntry { .. }
            | FirExprKind::ClassifierPropertyRead { .. }
            | FirExprKind::ValueRead(_)
            | FirExprKind::CapturedValueRead { .. }
            | FirExprKind::ClassStorageRead { .. }
            | FirExprKind::ConstructorCaptureRead { .. }
            | FirExprKind::ConstructorContextRead { .. }
            | FirExprKind::ClassStorageSharedRead { .. }
            | FirExprKind::ClassStorageSharedWrite { .. }
            | FirExprKind::ConstructorCaptureSharedWrite { .. }
            | FirExprKind::EnclosingClassStorageRead { .. }
            | FirExprKind::CapturedClassStorageRead { .. }
            | FirExprKind::CapturedClassStorageSharedWrite { .. }
            | FirExprKind::CapturedValueWrite { .. }
            | FirExprKind::ValueWrite { .. }
            | FirExprKind::LateinitFieldRead { .. }
            | FirExprKind::BackingFieldRead { .. }
            | FirExprKind::BackingFieldWrite { .. }
            | FirExprKind::PluginExpression { .. }
            | FirExprKind::AnonymousObject(_)
            | FirExprKind::LocalCall {
                extension_receiver: None,
                ..
            }
            | FirExprKind::ComparisonCall { .. }
            | FirExprKind::ContainmentCall { .. }
            | FirExprKind::FunctionInvokeReference { .. }
            | FirExprKind::ExtensionFunctionBinding { .. }
            | FirExprKind::CallableReference { .. }
            | FirExprKind::LocalCallableReference { .. }
            | FirExprKind::PropertyReference { .. }
            | FirExprKind::LocalPropertyReference { .. }
            | FirExprKind::ClassLiteral { .. }
            | FirExprKind::TypeOperation { .. }
            | FirExprKind::NullablePrimitiveComparison { .. }
            | FirExprKind::InRange { .. }
            | FirExprKind::IndexedRead { .. }
            | FirExprKind::IndexedWrite { .. }
            | FirExprKind::SafeCall { .. }
            | FirExprKind::Elvis { .. }
            | FirExprKind::StringTemplate(_)
            | FirExprKind::Throw(_)
            | FirExprKind::Jump { .. }
            | FirExprKind::Lambda { .. }
            | FirExprKind::Try { .. }
            | FirExprKind::Conditional { .. }
            | FirExprKind::When { .. }
            | FirExprKind::Block { .. } => None,
        }
    }

    fn receiver_function_argument(kind: &FirExprKind) -> Option<FirReceiver> {
        let FirExprKind::FunctionInvoke { arguments, .. } = kind else {
            return None;
        };
        arguments.iter().find_map(|argument| match argument {
            FirCallArgument::Expression {
                parameter: 0,
                value,
                ..
            } => Some(FirReceiver {
                value: *value,
                conversion: None,
            }),
            FirCallArgument::Expression { .. }
            | FirCallArgument::Default { .. }
            | FirCallArgument::Vararg { .. } => None,
        })
    }

    fn new<'a>(
        file: &'a File,
        info: &'a TypeInfo,
        source: SourceFileId,
        owner: BodyOwnerId,
        root_span: Span,
        index: &'a ResolvedModuleIndex,
        origins: &'a mut OriginStore,
        session: &'a mut BodyCheckSession,
    ) -> BodyFirChecker<'a> {
        let target_origin = origins.source(source, root_span);
        let mut body = FirBody::new(owner);
        let declaration = DeclarationId::from_raw(owner.raw());
        let lexical_class_owner = {
            let mut current = declaration;
            let mut first = true;
            loop {
                let Some(anchor) = index.declaration_anchor(current) else {
                    break None;
                };
                if anchor.kind == crate::fir::DeclarationKind::EnumEntry && first {
                    // Constructor arguments of the entry itself execute in the enclosing enum's
                    // initialization container. Members owned below the entry select it normally.
                    current = match anchor.owner {
                        Some(owner) => owner,
                        None => break None,
                    };
                    first = false;
                    continue;
                }
                if matches!(
                    anchor.kind,
                    crate::fir::DeclarationKind::Classifier
                        | crate::fir::DeclarationKind::EnumEntry
                ) {
                    break Some(current);
                }
                current = match anchor.owner {
                    Some(owner) => owner,
                    None => break None,
                };
                first = false;
            }
        };
        if lexical_class_owner.is_some() {
            body.set_lexical_class_owner(lexical_class_owner);
        }
        let owns_callable_result = index.declaration_anchor(declaration).is_some_and(|anchor| {
            matches!(
                anchor.kind,
                crate::fir::DeclarationKind::Function
                    | crate::fir::DeclarationKind::Property
                    | crate::fir::DeclarationKind::Accessor
            )
        });
        if let Some(signature) = owns_callable_result
            .then(|| index.signature(declaration))
            .flatten()
        {
            // Return checking needs the finalized declaration result before walking the body. The
            // driver may restate this fact after construction, but setting it here prevents return
            // conversions from depending on a post-check mutation.
            body.set_result_type(signature.result);
        }
        let return_target = body.add_control_target(FirControlTarget {
            origin: target_origin,
            kind: FirControlTargetKind::Body(owner),
        });
        let has_dispatch_receiver = index
            .enclosing_classifier(DeclarationId::from_raw(owner.raw()))
            .is_some();
        let enclosing_classifier = index.enclosing_classifier(DeclarationId::from_raw(owner.raw()));
        let constructor_capture_parameter_count = index
            .declaration_anchor(declaration)
            .filter(|anchor| {
                matches!(
                    anchor.kind,
                    crate::fir::DeclarationKind::Constructor
                        | crate::fir::DeclarationKind::Initializer
                )
            })
            .and_then(|_| enclosing_classifier)
            .map(|classifier| {
                let transient = session
                    .active_source
                    .as_ref()
                    .and_then(|active| {
                        active
                            .class(file, classifier.declaration)
                            .map(|(declaration, _)| declaration)
                    })
                    .or_else(|| {
                        let range = index.declaration_range(classifier.declaration)?;
                        file.decl_arena.iter().enumerate().find_map(|(raw, declaration)| {
                            if matches!(declaration, crate::ast::Decl::Class(class) if class.span == range)
                            {
                                Some(crate::ast::DeclId(u32::try_from(raw).ok()?))
                            } else {
                                None
                            }
                        })
                    });
                let captures = transient
                    .and_then(|declaration| {
                        info.local_class_captures_by_class
                            .get(&declaration)
                            .or_else(|| info.anonymous_object_captures_by_class.get(&declaration))
                    })
                    .map_or(0, Vec::len);
                let implicit_outer = index
                    .declaration_header(classifier.declaration)
                    .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::INNER));
                u32::try_from(captures)
                    .expect("too many checked classifier captures")
                    .checked_add(u32::from(implicit_outer))
                    .expect("too many constructor prefix parameters")
            })
            .unwrap_or(0);
        if constructor_capture_parameter_count != 0 {
            body.set_constructor_capture_parameter_count(constructor_capture_parameter_count);
        }
        let constructor_context_parameter_count = index
            .declaration_anchor(declaration)
            .filter(|anchor| {
                matches!(
                    anchor.kind,
                    crate::fir::DeclarationKind::Constructor
                        | crate::fir::DeclarationKind::Initializer
                )
            })
            .and_then(|_| enclosing_classifier)
            .and_then(|classifier| index.classifier_header(classifier.declaration))
            .map(|classifier| {
                u32::try_from(classifier.context_parameters.len())
                    .expect("too many classifier context parameters")
            })
            .unwrap_or(0);
        if constructor_context_parameter_count != 0 {
            body.set_constructor_context_parameter_count(constructor_context_parameter_count);
        }
        let mut class_context = enclosing_classifier
            .and_then(|classifier| session.class_bodies.get(&classifier.declaration))
            .cloned()
            .unwrap_or_default();
        let indexed_enclosing_property = {
            let mut declaration = declaration;
            loop {
                if let Some(property) = index.property_for_declaration(declaration) {
                    break Some(property);
                }
                let Some(owner) = index
                    .declaration_anchor(declaration)
                    .and_then(|anchor| anchor.owner)
                else {
                    break None;
                };
                declaration = owner;
            }
        };
        let enclosing_property = class_context
            .enclosing_property
            .or(indexed_enclosing_property);
        let mut enclosing = enclosing_classifier
            .and_then(|classifier| index.declaration_anchor(classifier.declaration))
            .and_then(|anchor| anchor.owner);
        let mut enclosing_depth = 1u32;
        while let Some(classifier) =
            enclosing.filter(|owner| index.classifier_header(*owner).is_some())
        {
            if let Some(outer) = session.class_bodies.get(&classifier) {
                for (name, binding) in &outer.values {
                    class_context
                        .values
                        .entry(name.clone())
                        .or_insert(ClassCaptureBinding {
                            enclosing_depth: binding
                                .enclosing_depth
                                .checked_add(enclosing_depth)
                                .expect("too many enclosing local classifiers"),
                            ..*binding
                        });
                }
                for (statement, (depth, callable)) in &outer.callables {
                    class_context.callables.entry(*statement).or_insert((
                        depth
                            .checked_add(enclosing_depth)
                            .expect("too many enclosing local classifier bodies"),
                        *callable,
                    ));
                }
                class_context
                    .receivers
                    .extend(outer.receivers.iter().map(|binding| {
                        ClassCaptureBinding {
                            enclosing_depth: binding
                                .enclosing_depth
                                .checked_add(enclosing_depth)
                                .expect("too many enclosing local classifiers"),
                            semantic_receiver_depth: binding.semantic_receiver_depth.map(|depth| {
                                depth
                                    .checked_add(enclosing_depth)
                                    .expect("too many enclosing receiver coordinates")
                            }),
                            ..*binding
                        }
                    }));
            }
            enclosing = index
                .declaration_anchor(classifier)
                .and_then(|anchor| anchor.owner);
            enclosing_depth = enclosing_depth
                .checked_add(1)
                .expect("too many enclosing local classifiers");
        }
        crate::trace_compiler!(
            "fir",
            "body capture context owner={:?} values={} delegates={} callables={} receivers={:?}",
            owner,
            class_context.values.len(),
            class_context.delegates.len(),
            class_context.callables.len(),
            class_context.receivers,
        );
        let streamed_outer_callables = class_context.callables.keys().copied().collect();
        BodyFirChecker {
            file,
            info,
            source,
            index,
            origins,
            body,
            scopes: vec![HashMap::new()],
            delegate_scopes: vec![HashMap::new()],
            outer_values: HashMap::new(),
            outer_delegates: HashMap::new(),
            class_values: class_context.values,
            class_delegates: class_context.delegates,
            class_receivers: class_context.receivers,
            enclosing_property,
            session,
            return_target,
            lambda_return_source: None,
            outer_lambda_return_depths: HashMap::new(),
            function_return_depth: 0,
            loops: Vec::new(),
            local_callable_scopes: vec![HashMap::new()],
            expression_substitutions: HashMap::new(),
            outer_callables: class_context.callables,
            streamed_outer_callables,
            nested_body_depth: 0,
            owned_receiver_count: u32::from(has_dispatch_receiver),
            outer_receiver_frames: Vec::new(),
            constructor_prefix_capture_access: false,
        }
    }

    fn configure_receivers(
        &mut self,
        context_receivers: &[ResolvedTy],
        context_value_count: u32,
        extension_receiver: Option<ResolvedTy>,
    ) {
        self.body
            .set_context_receiver_types(context_receivers.to_vec());
        self.body.set_context_value_count(context_value_count);
        if let Some(receiver) = extension_receiver {
            self.body.set_receiver_type(receiver);
        }
        self.owned_receiver_count = self
            .owned_receiver_count
            .checked_add(
                u32::try_from(context_receivers.len())
                    .expect("too many checked-body context parameters"),
            )
            .and_then(|count| count.checked_add(u32::from(extension_receiver.is_some())))
            .expect("too many checked-body implicit receivers");
    }

    /// Semantic receiver frame exposed to a nested callable. Direct callable receivers are ordinary
    /// slots. A non-local member body can additionally expose outer instances through an `inner`
    /// classifier chain; publish the exact declaration path for each such coordinate so a nested
    /// capture never degrades it to type/depth lookup in lowering.
    fn receiver_frame(&self) -> ReceiverFrame {
        let mut structural_paths = HashMap::new();
        let mut capture_depths = HashMap::new();
        let extension_count = u32::from(self.body.receiver_type().is_some());
        let context_count = u32::try_from(self.body.context_receiver_types().len())
            .expect("too many checked-body context receivers");
        let context_value_count = self.body.context_value_count().min(context_count);
        let mut semantic_depth = 0;
        let mut runtime_depth = 0;
        if extension_count != 0 {
            capture_depths.insert(semantic_depth, runtime_depth);
            semantic_depth += 1;
            runtime_depth += 1;
        }
        for declaration_ordinal in (0..context_count).rev() {
            if declaration_ordinal >= context_value_count {
                capture_depths.insert(semantic_depth, runtime_depth);
                runtime_depth += 1;
            }
            semantic_depth += 1;
        }
        if self.body.local_callable().is_some() {
            return ReceiverFrame {
                width: self.owned_receiver_count,
                dispatch_owner: None,
                dispatch_depth: None,
                capture_depths,
                structural_paths,
            };
        }
        let dispatch_owner = self.current_storage_owner();
        let dispatch_depth = dispatch_owner.map(|_| {
            capture_depths.insert(semantic_depth, runtime_depth);
            runtime_depth
        });
        let owner = DeclarationId::from_raw(self.body.owner().raw());
        let mut classifier = self
            .index
            .enclosing_classifier(owner)
            .map(|classifier| classifier.declaration);
        let mut path = Vec::new();
        while let Some(current) = classifier {
            let Some(header) = self.index.declaration_header(current) else {
                break;
            };
            if !header.flags.has(crate::fir::DeclarationFlags::INNER) {
                break;
            }
            let Some(outer) = self
                .index
                .declaration_anchor(current)
                .and_then(|anchor| anchor.owner)
                .filter(|owner| self.index.classifier_header(*owner).is_some())
            else {
                break;
            };
            path.push(current);
            let depth = self
                .owned_receiver_count
                .checked_add(
                    u32::try_from(structural_paths.len())
                        .expect("too many structural receiver paths"),
                )
                .expect("too many implicit receivers");
            structural_paths.insert(depth, path.clone().into_boxed_slice());
            if let Some(dispatch_depth) = dispatch_depth {
                capture_depths.insert(depth, dispatch_depth);
            }
            classifier = Some(outer);
        }
        ReceiverFrame {
            width: self
                .owned_receiver_count
                .checked_add(
                    u32::try_from(structural_paths.len())
                        .expect("too many structural receiver paths"),
                )
                .expect("too many implicit receivers"),
            dispatch_owner,
            dispatch_depth,
            capture_depths,
            structural_paths,
        }
    }

    /// The nearest stable declaration that owns instance storage for this body. This is a checked
    /// ownership edge, not a classifier/name search: entry-body members point directly at their
    /// stable enum-entry declaration, while ordinary members point at a classifier declaration.
    fn current_storage_owner(&self) -> Option<DeclarationId> {
        let mut declaration = DeclarationId::from_raw(self.body.owner().raw());
        loop {
            let anchor = self.index.declaration_anchor(declaration)?;
            if matches!(
                anchor.kind,
                crate::fir::DeclarationKind::Classifier | crate::fir::DeclarationKind::EnumEntry
            ) {
                return Some(declaration);
            }
            declaration = anchor.owner?;
        }
    }

    fn enclosing_receiver_capture(
        &self,
        receiver_depth: usize,
    ) -> Option<(u32, u32, Box<[DeclarationId]>)> {
        let mut depth = receiver_depth.checked_sub(self.owned_receiver_count as usize)?;
        for (enclosing_depth, frame) in self.outer_receiver_frames.iter().enumerate() {
            if depth < frame.width as usize {
                let semantic_depth = u32::try_from(depth).ok()?;
                let captured_depth = *frame.capture_depths.get(&semantic_depth)?;
                return Some((
                    u32::try_from(enclosing_depth).expect("too many nested receiver frames"),
                    captured_depth,
                    frame
                        .structural_paths
                        .get(&semantic_depth)
                        .cloned()
                        .unwrap_or_default(),
                ));
            }
            depth = depth.checked_sub(frame.width as usize)?;
        }
        None
    }

    /// Translate a resolver receiver-tower coordinate beyond this callable's own receiver slots
    /// into the exact semantic `inner`-classifier path that supplies it at runtime. This publishes
    /// declaration identities only; how a backend stores each enclosing instance is deliberately
    /// absent from checked FIR.
    fn enclosing_receiver_path(
        &self,
        selected: &crate::resolve::ImplicitReceiverSelection,
    ) -> Option<Box<[DeclarationId]>> {
        // An enum-entry body exposes both the anonymous entry receiver and its parent-enum view as
        // receiver-tower rungs, but they are the same runtime dispatch object. Publish that exact
        // alias as the zero-edge enclosing path; lowering then reads the current dispatch slot and
        // performs no tower interpretation of its own.
        if let Some(entry) = self.current_storage_owner().filter(|owner| {
            self.index
                .declaration_anchor(*owner)
                .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::EnumEntry)
        }) {
            let parent = self
                .index
                .declaration_anchor(entry)?
                .owner
                .and_then(|owner| self.index.classifier_header(owner))?;
            if selected.ty.non_null().kotlin_class_internal() == Some(parent.classifier) {
                return Some(Box::new([]));
            }
        }
        selected
            .receiver_depth
            .checked_sub(self.owned_receiver_count as usize)?;
        let owner = DeclarationId::from_raw(self.body.owner().raw());
        let mut classifier = self.index.enclosing_classifier(owner)?.declaration;
        let selected_classifier = selected.classifier;
        let selected_type = selected.ty.non_null().kotlin_class_internal()?;
        let matches_selected = |candidate: DeclarationId| {
            selected_classifier == Some(candidate)
                || self
                    .index
                    .classifier_header(candidate)
                    .is_some_and(|header| header.classifier == selected_type)
                || self
                    .index
                    .declaration_anchor(candidate)
                    .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::EnumEntry)
                    .and_then(|anchor| anchor.owner)
                    .is_some_and(|parent| {
                        selected_classifier == Some(parent)
                            || self
                                .index
                                .classifier_header(parent)
                                .is_some_and(|header| header.classifier == selected_type)
                    })
        };
        let mut path = Vec::new();
        loop {
            if matches_selected(classifier) {
                return Some(path.into_boxed_slice());
            }
            let header = self.index.declaration_header(classifier)?;
            if !header.flags.has(crate::fir::DeclarationFlags::INNER) {
                return None;
            }
            let outer = self
                .index
                .declaration_anchor(classifier)?
                .owner
                .filter(|owner| {
                    self.index.classifier_header(*owner).is_some()
                        || self.index.declaration_anchor(*owner).is_some_and(|anchor| {
                            anchor.kind == crate::fir::DeclarationKind::EnumEntry
                        })
                })?;
            path.push(classifier);
            classifier = outer;
        }
    }

    fn failure(&self, span: Option<Span>, kind: BodyCheckFailureKind) -> BodyCheckFailure {
        BodyCheckFailure { span, kind }
    }

    fn builtin_binary_expression(
        &mut self,
        expression: ExprId,
        operation: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let lhs_type = self.expression_type(lhs)?;
        let rhs_type = self.expression_type(rhs)?;
        let promotes_operands = matches!(
            operation,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Rem
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
        );
        let operation = match operation {
            BinOp::Add => FirBinaryOperation::Add,
            BinOp::Sub => FirBinaryOperation::Subtract,
            BinOp::Mul => FirBinaryOperation::Multiply,
            BinOp::Div => FirBinaryOperation::Divide,
            BinOp::Rem => FirBinaryOperation::Remainder,
            BinOp::Eq => FirBinaryOperation::Equal,
            BinOp::Ne => FirBinaryOperation::NotEqual,
            BinOp::Lt => FirBinaryOperation::Less,
            BinOp::Le => FirBinaryOperation::LessOrEqual,
            BinOp::Gt => FirBinaryOperation::Greater,
            BinOp::Ge => FirBinaryOperation::GreaterOrEqual,
            BinOp::And => FirBinaryOperation::BooleanAnd,
            BinOp::Or => FirBinaryOperation::BooleanOr,
            BinOp::RefEq => FirBinaryOperation::ReferentialEqual,
            BinOp::RefNe => FirBinaryOperation::ReferentialNotEqual,
        };
        self.checked_binary_expression(
            expression,
            operation,
            promotes_operands,
            lhs,
            rhs,
            lhs_type,
            rhs_type,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn checked_binary_expression(
        &mut self,
        expression: ExprId,
        operation: FirBinaryOperation,
        promotes_operands: bool,
        lhs: ExprId,
        rhs: ExprId,
        lhs_type: ResolvedTy,
        rhs_type: ResolvedTy,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let promoted = promotes_operands
            .then(|| Ty::promote(lhs_type.get(), rhs_type.get()))
            .flatten()
            .map(|ty| {
                self.resolved_type(
                    self.file
                        .expr_span(expression)
                        .expect("a checked binary expression has a source span"),
                    ty,
                )
            })
            .transpose()?;
        let operand = |checker: &mut Self,
                       source: ExprId,
                       actual: ResolvedTy|
         -> Result<FirExprId, BodyCheckFailure> {
            let value = checker.expression(source)?;
            let Some(target) = promoted else {
                return Ok(value);
            };
            let cause = checker.expression_origin(source)?;
            let Some(conversion) = checker.selected_type_conversion(actual, target, cause) else {
                return Ok(value);
            };
            Ok(checker.body.add_expr(FirExpr {
                origin: cause,
                ty: target,
                kind: FirExprKind::ImplicitConversion { value, conversion },
            }))
        };
        Ok(FirExprKind::Binary {
            operation,
            lhs: operand(self, lhs, lhs_type)?,
            rhs: operand(self, rhs, rhs_type)?,
        })
    }

    fn bind_local(&mut self, name: &str, ty: ResolvedTy) -> LocalValueId {
        let local = self.allocate_local();
        self.scopes
            .last_mut()
            .expect("a FIR checker always owns a lexical scope")
            .insert(name.to_owned(), LocalBinding { value: local, ty });
        local
    }

    fn allocate_local(&mut self) -> LocalValueId {
        self.body.allocate_local_value()
    }

    fn local(&self, name: &str) -> Option<LocalValueId> {
        self.local_binding(name).map(|binding| binding.value)
    }

    fn local_binding(&self, name: &str) -> Option<LocalBinding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn local_delegate(&self, name: &str) -> Option<LocalDelegateBinding> {
        self.delegate_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn delegated_binding(&self, name: &str) -> Option<(u32, LocalDelegateBinding)> {
        self.local_delegate(name)
            .map(|binding| (u32::MAX, binding))
            .or_else(|| self.outer_delegates.get(name).cloned())
            .or_else(|| {
                self.class_delegates
                    .get(name)
                    .cloned()
                    .map(|binding| (u32::MAX, binding))
            })
    }

    fn binding_source_at_shadow_depth(
        &self,
        name: &str,
        shadow_depth: usize,
    ) -> Option<(Option<u32>, LocalBinding)> {
        self.scopes
            .iter()
            .rev()
            .filter_map(|scope| scope.get(name).copied().map(|binding| (None, binding)))
            .chain(
                self.outer_values
                    .get(name)
                    .map(|(depth, binding)| (Some(*depth), *binding))
                    .into_iter(),
            )
            .nth(shadow_depth)
    }

    fn selected_operator(&self, expression: ExprId, name: &str) -> bool {
        self.info.resolved_operator_call(expression, name).is_some()
    }

    fn loop_target(&self, label: Option<&str>) -> Option<(u32, ControlTargetId)> {
        match label {
            Some(label) => self.loops.iter().rev().find_map(|(active, depth, target)| {
                (active.as_deref() == Some(label)).then_some((*depth, *target))
            }),
            None => self
                .loops
                .last()
                .map(|(_, depth, target)| (*depth, *target)),
        }
    }

    fn checked_loop_body(
        &mut self,
        target: ControlTargetId,
        label: &Option<String>,
        binding: Option<(&str, LocalBinding)>,
        body: ExprId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        self.loops.push((label.clone(), 0, target));
        self.scopes.push(HashMap::new());
        self.delegate_scopes.push(HashMap::new());
        if let Some((name, value)) = binding {
            self.scopes
                .last_mut()
                .expect("a loop body owns a lexical scope")
                .insert(name.to_owned(), value);
        }
        let result = self.expression(body);
        self.delegate_scopes.pop();
        self.scopes.pop();
        self.loops.pop();
        result
    }

    fn checked_do_while(
        &mut self,
        target: ControlTargetId,
        label: &Option<String>,
        body: ExprId,
        condition: ExprId,
    ) -> Result<(FirExprId, FirExprId), BodyCheckFailure> {
        self.loops.push((label.clone(), 0, target));
        self.scopes.push(HashMap::new());
        self.delegate_scopes.push(HashMap::new());
        self.local_callable_scopes.push(HashMap::new());
        let result = (|| {
            let checked_body = match self.file.expr(body).clone() {
                Expr::Block { stmts, trailing } => {
                    let kind = self.block_in_current_scope(&stmts, trailing)?;
                    self.add_expression(body, kind)?
                }
                _ => self.expression(body)?,
            };
            let checked_condition = self.expression(condition)?;
            Ok((checked_body, checked_condition))
        })();
        self.local_callable_scopes.pop();
        self.delegate_scopes.pop();
        self.scopes.pop();
        self.loops.pop();
        result
    }

    fn increment_value(
        &mut self,
        expression: ExprId,
        target: ExprId,
        decrement: bool,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let convention = if decrement { "dec" } else { "inc" };
        let kind = if self.selected_operator(expression, convention) {
            self.zero_arg_expression_operator_call(expression, convention, target)?
        } else {
            FirExprKind::Unary {
                operation: if decrement {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand: self.expression(target)?,
            }
        };
        self.add_expression(expression, kind)
    }

    fn increment_local_expression(
        &mut self,
        expression: ExprId,
        target: ExprId,
        decrement: bool,
        prefix: bool,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Expr::Name(name) = self.file.expr(target) else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
            ));
        };
        if let Some((depth, delegate)) = self.delegated_binding(name) {
            return self.delegated_inc_dec_expression(
                expression, target, decrement, prefix, depth, delegate,
            );
        }
        let local = self.local(name);
        let captured = if local.is_none() {
            self.outer_values.get(name).copied()
        } else {
            None
        };
        let class_storage = if local.is_none() && captured.is_none() {
            self.class_values.get(name).copied()
        } else {
            None
        };
        if local.is_none() && captured.is_none() && class_storage.is_none() {
            if let Some(kind) =
                self.property_inc_dec_expression(expression, target, decrement, prefix)?
            {
                return Ok(kind);
            }
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnknownLocal,
            ));
        }
        if let Some((enclosing_depth, binding)) = captured {
            let cause = self.expression_origin(expression)?;
            self.body.add_capture(FirCapture {
                origin: cause,
                enclosing_depth,
                source: binding.value,
                ty: binding.ty,
                shared_cell: true,
            });
        }
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Expression(expression))
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
                )
            })?;
        let cause = self.expression_origin(expression)?;
        let mut statements = Vec::new();
        let old_value = if prefix {
            None
        } else {
            let temporary = self.allocate_local();
            let read_kind = match (local, captured, class_storage) {
                (Some(local), _, _) => FirExprKind::ValueRead(local),
                (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueRead {
                    enclosing_depth,
                    source: binding.value,
                },
                (None, None, Some(binding)) => self.class_storage_read_kind(binding, cause)?,
                (None, None, None) => unreachable!("increment target was checked above"),
            };
            let read = self.body.add_expr(FirExpr {
                origin: cause,
                ty: self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    resolution.receiver_ty,
                )?,
                kind: read_kind,
            });
            statements.push(self.body.add_statement(FirStatement {
                origin: cause,
                kind: FirStatementKind::Local {
                    target: temporary,
                    ty: self.resolved_type(
                        self.file.expr_span(expression).ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                        resolution.receiver_ty,
                    )?,
                    mutable: false,
                    lateinit: false,
                    initializer: Some(read),
                    conversion: None,
                },
            }));
            Some(temporary)
        };
        let updated = self.increment_value(expression, target, decrement)?;
        let write_kind = match (local, captured, class_storage) {
            (Some(local), _, _) => FirExprKind::ValueWrite {
                target: local,
                value: updated,
                conversion: None,
            },
            (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueWrite {
                enclosing_depth,
                source: binding.value,
                value: updated,
                conversion: None,
            },
            (None, None, Some(binding)) => {
                self.class_storage_shared_write_kind(binding, cause, updated, None)?
            }
            (None, None, None) => unreachable!("increment target was checked above"),
        };
        let write = self.body.add_expr(FirExpr {
            origin: cause,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
            kind: write_kind,
        });
        statements.push(self.body.add_statement(FirStatement {
            origin: cause,
            kind: FirStatementKind::Expression(write),
        }));
        let result_kind = match (old_value, local, captured, class_storage) {
            (Some(old_value), _, _, _) => FirExprKind::ValueRead(old_value),
            (None, Some(local), _, _) => FirExprKind::ValueRead(local),
            (None, None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueRead {
                enclosing_depth,
                source: binding.value,
            },
            (None, None, None, Some(binding)) => self.class_storage_read_kind(binding, cause)?,
            (None, None, None, None) => unreachable!("increment target was checked above"),
        };
        let result = self.body.add_expr(FirExpr {
            origin: cause,
            ty: self.expression_type(expression)?,
            kind: result_kind,
        });
        Ok(FirExprKind::Block {
            statements: statements.into_boxed_slice(),
            result: Some(result),
        })
    }

    fn range_operation(kind: RangeKind) -> FirRangeOperation {
        match kind {
            RangeKind::Through => FirRangeOperation::Through,
            RangeKind::OpenEnd => FirRangeOperation::OpenEnd,
            RangeKind::Until => FirRangeOperation::Until,
            RangeKind::DownTo => FirRangeOperation::DownTo,
        }
    }

    fn safe_selector_type(&self, expression: ExprId) -> Option<Ty> {
        self.info
            .resolved_calls
            .get(&expression)
            .map(crate::resolve::ResolvedCall::ret)
            .or_else(|| {
                self.info
                    .resolved_constructor(expression)
                    .map(|constructor| Ty::obj_name(constructor.owner()))
            })
            .or_else(|| match self.info.expr_lowers.get(&expression) {
                Some(ExprLowering::TopLevelPropertyGet(access))
                | Some(ExprLowering::ExtensionPropertyGet { access }) => Some(access.property.ty),
                Some(ExprLowering::MemberPropertyRead { declaration_ty, .. }) => {
                    Some(*declaration_ty)
                }
                Some(ExprLowering::MemberExtensionPropertyRead { ty, .. }) => Some(*ty),
                Some(ExprLowering::AssociatedPropertyRead { .. }) => {
                    Some(self.info.semantic_ty(expression))
                }
                Some(ExprLowering::EnumEntryPropertyRead { .. }) => {
                    Some(self.info.semantic_ty(expression))
                }
                Some(
                    ExprLowering::BuiltinUnaryCall { .. }
                    | ExprLowering::RuntimeTypeOperand(_)
                    | ExprLowering::ExtensionFunctionBinding { .. }
                    | ExprLowering::PluginExpression(_)
                    | ExprLowering::ClassStorageRead { .. }
                    | ExprLowering::BackingFieldRead
                    | ExprLowering::ImplicitPropertyIncDec(_)
                    | ExprLowering::LateinitInitialized { .. }
                    | ExprLowering::LocalFunction { .. }
                    | ExprLowering::AdaptedLocalFunctionRef { .. }
                    | ExprLowering::ConstructorRef { .. }
                    | ExprLowering::TopLevelFunctionRef(_)
                    | ExprLowering::CallableReference { .. }
                    | ExprLowering::AdaptedCallableReference { .. }
                    | ExprLowering::FunctionInvokeReference { .. }
                    | ExprLowering::AdaptedRef { .. }
                    | ExprLowering::SamConstructorReference { .. }
                    | ExprLowering::UnavailableCallableReference { .. }
                    | ExprLowering::Unavailable { .. }
                    | ExprLowering::Erased
                    | ExprLowering::IncDecAccessOperands(_)
                    | ExprLowering::TopLevelPropertyIncDec(_)
                    | ExprLowering::CompilerSynthetic(_)
                    | ExprLowering::SamConstructor { .. }
                    | ExprLowering::Lambda(_)
                    | ExprLowering::SingletonValue(_)
                    | ExprLowering::ClassifierPropertyRead { .. }
                    | ExprLowering::LabeledThisInner
                    | ExprLowering::LabeledThisDispatch
                    | ExprLowering::IntrinsicProperty(_)
                    | ExprLowering::Invoke { .. }
                    | ExprLowering::SafePropertyInvoke { .. }
                    | ExprLowering::ClassLiteral { .. }
                    | ExprLowering::ReceiverFnInvoke { .. },
                )
                | None => None,
            })
    }

    fn expression(&mut self, expression: ExprId) -> Result<FirExprId, BodyCheckFailure> {
        if let Some(replacement) = self.expression_substitutions.get(&expression).copied() {
            return Ok(replacement);
        }
        if let Some(ExprLowering::PluginExpression(plan)) =
            self.info.expr_lowers.get(&expression).cloned()
        {
            return self.plugin_expression(expression, *plan);
        }
        if matches!(
            self.info.expr_lowers.get(&expression),
            Some(ExprLowering::Erased)
        ) {
            // A trailing `contract { ... }` is an expression-position compile-time declaration.
            // Resolution has already identified the intrinsic and decoded its effects; checked FIR
            // keeps only its semantic Unit result and never checks or lowers the DSL lambda.
            return self.add_expression_with_type(
                expression,
                ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                FirExprKind::Block {
                    statements: Box::new([]),
                    result: None,
                },
            );
        }
        // A checked `const val` initializer may be an entire expression tree (`2 + 2`, a template,
        // or a chain of constant reads). Resolution has already evaluated it from exact semantic
        // selections, so publish the root payload directly. A MEMBER read remains on its dedicated
        // path below because an ordinary value receiver must still be evaluated for effects.
        let folded = (!matches!(self.file.expr(expression), Expr::Member { .. }))
            .then(|| self.info.resolved_constants.get(&expression))
            .flatten();
        let kind = if let Some(constant) = folded {
            FirExprKind::Constant(checked_constant_value(constant))
        } else {
            match self.file.expr(expression) {
                Expr::IntLit(value) => {
                    let constant = match self.expression_type(expression)?.get() {
                        Ty::Long => FirConstant::Long(*value),
                        Ty::UInt => FirConstant::UInt(*value),
                        Ty::ULong => FirConstant::ULong(*value),
                        _ => FirConstant::Int(*value),
                    };
                    FirExprKind::Constant(constant)
                }
                Expr::LongLit(value) => FirExprKind::Constant(FirConstant::Long(*value)),
                Expr::UIntLit(value) => FirExprKind::Constant(FirConstant::UInt(*value)),
                Expr::ULongLit(value) => FirExprKind::Constant(FirConstant::ULong(*value)),
                Expr::DoubleLit(value) => FirExprKind::Constant(FirConstant::Double(*value)),
                Expr::FloatLit(value) => FirExprKind::Constant(FirConstant::Float(*value)),
                Expr::BoolLit(value) => FirExprKind::Constant(FirConstant::Boolean(*value)),
                Expr::StringLit(value) => FirExprKind::Constant(FirConstant::String(value.clone())),
                Expr::CharLit(value) => FirExprKind::Constant(FirConstant::Char(*value)),
                Expr::NullLit => FirExprKind::Constant(FirConstant::Null),
                Expr::AnnotationArrayLiteral(elements) => {
                    let elements = elements
                        .iter()
                        .map(|element| self.expression(*element))
                        .collect::<Result<Vec<_>, _>>()?;
                    FirExprKind::AnnotationArray(elements.into_boxed_slice())
                }
                Expr::UnsupportedAnnotationArgument(_) => {
                    return Err(self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::InvalidAnnotationArgument,
                    ));
                }
                Expr::Name(name) => {
                    if name == "this"
                        && !matches!(
                            self.info.expr_lowers.get(&expression),
                            Some(ExprLowering::SingletonValue(_))
                        )
                    {
                        return self
                            .implicit_receiver(expression)?
                            .map(|receiver| receiver.value)
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::UnknownLocal,
                                )
                            });
                    } else if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::LabeledThisInner)
                    ) {
                        return self
                            .implicit_receiver(expression)?
                            .map(|receiver| receiver.value)
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::UnknownLocal,
                                )
                            });
                    } else if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::LabeledThisDispatch)
                    ) {
                        return self
                            .implicit_receiver(expression)?
                            .map(|receiver| receiver.value)
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::UnknownLocal,
                                )
                            });
                    } else if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::BackingFieldRead)
                    ) {
                        FirExprKind::BackingFieldRead {
                            target: self.enclosing_property(expression)?,
                        }
                    } else if let Some(ExprLowering::ClassStorageRead { field }) =
                        self.info.expr_lowers.get(&expression)
                    {
                        match self.class_values.get(name).copied() {
                            Some(binding) => {
                                let origin = self.expression_origin(expression)?;
                                self.class_storage_read_kind(binding, origin)?
                            }
                            None => {
                                let field = *field;
                                let origin = self.expression_origin(expression)?;
                                let ty = self.expression_type(expression)?;
                                self.direct_class_storage_read_kind(field, ty, origin)?
                            }
                        }
                    } else if let Some(ExprLowering::SingletonValue(singleton)) =
                        self.info.expr_lowers.get(&expression)
                    {
                        FirExprKind::SingletonValue {
                            classifier: singleton.classifier,
                        }
                    } else if let Some(entry) = self.info.resolved_enum_entry(expression) {
                        FirExprKind::EnumEntry {
                            classifier: entry.classifier,
                            ordinal: entry.ordinal,
                            name: entry.name.clone().into_boxed_str(),
                        }
                    } else if let Some((depth, delegate)) = self.delegated_binding(name) {
                        return self.delegated_read(expression, depth, delegate);
                    } else if let Some(local) = self.local_binding(name) {
                        return self.checked_storage_read(
                            expression,
                            local.ty,
                            FirExprKind::ValueRead(local.value),
                        );
                    } else if let Some((enclosing_depth, source)) =
                        self.outer_values.get(name).copied()
                    {
                        let origin = self.expression_origin(expression)?;
                        self.body.add_capture(FirCapture {
                            origin,
                            enclosing_depth,
                            source: source.value,
                            ty: source.ty,
                            shared_cell: false,
                        });
                        return self.checked_storage_read(
                            expression,
                            source.ty,
                            FirExprKind::CapturedValueRead {
                                enclosing_depth,
                                source: source.value,
                            },
                        );
                    } else if let Some(binding) = self.class_values.get(name).copied() {
                        let origin = self.expression_origin(expression)?;
                        let kind = self.class_storage_read_kind(binding, origin)?;
                        return self.checked_storage_read(expression, binding.ty, kind);
                    } else if let Some(constant) = self.info.resolved_constants.get(&expression) {
                        // A `const val` referenced by BARE NAME from inside its own classifier. The
                        // checker folds it exactly as it folds a qualified one; there is no property to
                        // read at run time.
                        FirExprKind::Constant(checked_constant_value(constant))
                    } else if let Some(property) = self.source_property_read(expression, None)? {
                        property
                    } else {
                        return Err(self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnknownLocal,
                        ));
                    }
                }
                Expr::NotNull { operand } => FirExprKind::TypeOperation {
                    operation: FirTypeOperation::NotNullAssertion,
                    operand: self.expression(*operand)?,
                    target: self.expression_type(expression)?,
                },
                Expr::Elvis { lhs, rhs } => FirExprKind::Elvis {
                    lhs: self.expression(*lhs)?,
                    rhs: self.expression(*rhs)?,
                },
                Expr::Template(parts) => {
                    let cause = self.expression_origin(expression)?;
                    let mut values = Vec::with_capacity(parts.len());
                    for part in parts {
                        values.push(match part {
                            TemplatePart::Expr(value) => {
                                let value_type = self.expression_type(*value)?;
                                let value = self.expression(*value)?;
                                if value_type.get().is_unsigned() {
                                    let origin = self.origins.synthetic(
                                        cause,
                                        SyntheticOriginKind::GeneratedControlFlow,
                                    );
                                    self.body.add_expr(FirExpr {
                                        origin,
                                        ty: ResolvedTy::new(Ty::String)
                                            .expect("String is a publishable FIR type"),
                                        kind: FirExprKind::Call(FirCall {
                                            target: FirCallTarget::Intrinsic {
                                                operation: FirIntrinsic::UnsignedToString {
                                                    source: value_type,
                                                },
                                                receiver: Some(value_type),
                                                parameters: Box::new([]),
                                                result: ResolvedTy::new(Ty::String)
                                                    .expect("String is a publishable FIR type"),
                                            },
                                            dispatch_receiver: Some(FirReceiver {
                                                value,
                                                conversion: None,
                                            }),
                                            extension_receiver: None,
                                            parameter_types: Box::new([]),
                                            arguments: Box::new([]),
                                            substitutions: Box::new([]),
                                        }),
                                    })
                                } else {
                                    value
                                }
                            }
                            TemplatePart::Str(value) => {
                                let origin = self
                                    .origins
                                    .synthetic(cause, SyntheticOriginKind::StringTemplateLiteral);
                                self.body.add_expr(FirExpr {
                                    origin,
                                    ty: ResolvedTy::new(Ty::String)
                                        .expect("String is a publishable FIR type"),
                                    kind: FirExprKind::Constant(FirConstant::String(value.clone())),
                                })
                            }
                        });
                    }
                    FirExprKind::StringTemplate(values.into_boxed_slice())
                }
                Expr::Throw { operand } => FirExprKind::Throw(self.expression(*operand)?),
                Expr::Return { value, label } => {
                    let target = self
                        .info
                        .expr_return_targets
                        .get(&expression)
                        .copied()
                        .or_else(|| label.is_none().then(|| self.default_return_target()));
                    let Some(target_depth) = self.return_target_depth(target.as_ref()) else {
                        return Err(self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
                        ));
                    };
                    FirExprKind::Jump {
                        kind: FirJumpKind::Return { target_depth },
                        target: self.return_target,
                        value: value
                            .map(|value| self.return_value(value, target.as_ref()))
                            .transpose()?,
                    }
                }
                Expr::Is {
                    operand,
                    ty,
                    negated,
                } => {
                    let target = self.info.resolved_type(ty).ok_or_else(|| {
                        self.failure(Some(ty.span), BodyCheckFailureKind::UnresolvedTypeSyntax)
                    })?;
                    FirExprKind::TypeOperation {
                        operation: if *negated {
                            FirTypeOperation::NotIs
                        } else {
                            FirTypeOperation::Is
                        },
                        operand: self.expression(*operand)?,
                        target: self.resolved_type(ty.span, target)?,
                    }
                }
                Expr::As {
                    operand,
                    ty,
                    nullable,
                } => {
                    let target = self.info.resolved_type(ty).ok_or_else(|| {
                        self.failure(Some(ty.span), BodyCheckFailureKind::UnresolvedTypeSyntax)
                    })?;
                    FirExprKind::TypeOperation {
                        operation: if *nullable {
                            FirTypeOperation::SafeCast
                        } else {
                            FirTypeOperation::Cast
                        },
                        operand: self.expression(*operand)?,
                        target: self.resolved_type(ty.span, target)?,
                    }
                }
                Expr::Unary { op, operand } => {
                    if self.selected_operator(expression, op.operator_name()) {
                        self.source_member_operator_call(
                            expression,
                            op.operator_name(),
                            *operand,
                            &[],
                        )?
                    } else {
                        FirExprKind::Unary {
                            operation: match op {
                                UnOp::Neg => FirUnaryOperation::Negate,
                                UnOp::Not => FirUnaryOperation::BooleanNot,
                                UnOp::Plus => FirUnaryOperation::Identity,
                            },
                            operand: self.expression(*operand)?,
                        }
                    }
                }
                Expr::Binary { op, lhs, rhs, .. } => {
                    let selected_name = match op {
                        BinOp::Add => Some("plus"),
                        BinOp::Sub => Some("minus"),
                        BinOp::Mul => Some("times"),
                        BinOp::Div => Some("div"),
                        BinOp::Rem => Some("rem"),
                        BinOp::Eq | BinOp::Ne => Some("equals"),
                        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some("compareTo"),
                        BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => None,
                    };
                    if let Some(operation) = selected_name
                        .and_then(|name| self.selected_primitive_binary_operation(expression, name))
                    {
                        self.checked_binary_expression(
                            expression,
                            operation,
                            true,
                            *lhs,
                            *rhs,
                            self.expression_type(*lhs)?,
                            self.expression_type(*rhs)?,
                        )?
                    } else if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
                        && self.selected_ieee_relational_operation(expression)
                    {
                        // The operator is already selected and checked. Primitive floating
                        // relations use IEEE ordering directly; an explicit `.compareTo()` remains
                        // a selected intrinsic call returning Int.
                        self.builtin_binary_expression(expression, *op, *lhs, *rhs)?
                    } else if selected_name
                        .is_some_and(|name| self.selected_operator(expression, name))
                    {
                        let convention = selected_name.expect("selected operator has a convention");
                        if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                            let selected = self.source_member_operator_call(
                                expression,
                                convention,
                                *lhs,
                                &[*rhs],
                            )?;
                            let FirExprKind::Call(call) = selected else {
                                unreachable!("a selected member operator always produces a call")
                            };
                            FirExprKind::ComparisonCall {
                                operation: match op {
                                    BinOp::Lt => FirBinaryOperation::Less,
                                    BinOp::Le => FirBinaryOperation::LessOrEqual,
                                    BinOp::Gt => FirBinaryOperation::Greater,
                                    BinOp::Ge => FirBinaryOperation::GreaterOrEqual,
                                    BinOp::Add
                                    | BinOp::Sub
                                    | BinOp::Mul
                                    | BinOp::Div
                                    | BinOp::Rem
                                    | BinOp::Eq
                                    | BinOp::Ne
                                    | BinOp::And
                                    | BinOp::Or
                                    | BinOp::RefEq
                                    | BinOp::RefNe => {
                                        unreachable!("only relational operators use compareTo FIR")
                                    }
                                },
                                call,
                            }
                        } else {
                            self.source_member_operator_call(expression, convention, *lhs, &[*rhs])?
                        }
                    } else if matches!(op, BinOp::Eq | BinOp::Ne) {
                        let lhs_ty = self.info.semantic_ty(*lhs);
                        let rhs_ty = self.info.semantic_ty(*rhs);
                        let nullable_primitive = lhs_ty
                            .nullable_primitive()
                            .filter(|primitive| *primitive == rhs_ty)
                            .map(|primitive| (*lhs, *rhs, primitive))
                            .or_else(|| {
                                rhs_ty
                                    .nullable_primitive()
                                    .filter(|primitive| *primitive == lhs_ty)
                                    .map(|primitive| (*rhs, *lhs, primitive))
                            });
                        if let Some((nullable, primitive, primitive_ty)) = nullable_primitive {
                            FirExprKind::NullablePrimitiveComparison {
                                operation: if *op == BinOp::Eq {
                                    FirBinaryOperation::Equal
                                } else {
                                    FirBinaryOperation::NotEqual
                                },
                                nullable: self.expression(nullable)?,
                                primitive: self.expression(primitive)?,
                                primitive_ty: self.resolved_type(
                                    self.file.expr_span(primitive).ok_or_else(|| {
                                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                                    })?,
                                    primitive_ty,
                                )?,
                            }
                        } else {
                            self.builtin_binary_expression(expression, *op, *lhs, *rhs)?
                        }
                    } else {
                        self.builtin_binary_expression(expression, *op, *lhs, *rhs)?
                    }
                }
                Expr::RangeTo { lo, hi, kind } => {
                    let convention = match kind {
                        RangeKind::Through => "rangeTo",
                        RangeKind::OpenEnd => "rangeUntil",
                        RangeKind::Until => "until",
                        RangeKind::DownTo => "downTo",
                    };
                    if self.selected_operator(expression, convention)
                        && !self.selected_range_construction(expression, convention)
                    {
                        self.source_member_operator_call(expression, convention, *lo, &[*hi])?
                    } else {
                        FirExprKind::Range {
                            operation: Self::range_operation(*kind),
                            start: self.expression(*lo)?,
                            start_type: self.expression_type(*lo)?,
                            end: self.expression(*hi)?,
                            end_type: self.expression_type(*hi)?,
                        }
                    }
                }
                Expr::IncDec {
                    target,
                    dec,
                    prefix,
                } => self.increment_local_expression(expression, *target, *dec, *prefix)?,
                Expr::InRange {
                    value,
                    start,
                    end,
                    kind,
                    negated,
                } => {
                    let selected_range = self.selected_operator(expression, "rangeTo");
                    let selected_contains = self.selected_operator(expression, "contains");
                    if selected_range && selected_contains {
                        let range_kind = if self.selected_range_construction(expression, "rangeTo")
                        {
                            FirExprKind::Range {
                                operation: Self::range_operation(*kind),
                                start: self.expression(*start)?,
                                start_type: self.expression_type(*start)?,
                                end: self.expression(*end)?,
                                end_type: self.expression_type(*end)?,
                            }
                        } else {
                            self.source_member_operator_call(
                                expression,
                                "rangeTo",
                                *start,
                                &[*end],
                            )?
                        };
                        let range_ty = self
                            .info
                            .resolved_operator_call(expression, "rangeTo")
                            .map(|call| call.ret())
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::UnsupportedExpression(
                                        ExpressionForm::InRange,
                                    ),
                                )
                            })?;
                        let origin = self.expression_origin(expression)?;
                        let range = self.body.add_expr(FirExpr {
                            origin,
                            ty: self.resolved_type(
                                self.file.expr_span(expression).ok_or_else(|| {
                                    self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                                })?,
                                range_ty,
                            )?,
                            kind: range_kind,
                        });
                        FirExprKind::ContainmentCall {
                            call: self.source_member_operator_call_on_value(
                                expression,
                                "contains",
                                range,
                                &[*value],
                            )?,
                            negated: *negated,
                        }
                    } else if selected_range || selected_contains {
                        return Err(self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::InRange),
                        ));
                    } else {
                        let comparison = self
                            .info
                            .resolved_in_range_comparisons
                            .get(&expression)
                            .copied()
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::UnsupportedExpression(
                                        ExpressionForm::InRange,
                                    ),
                                )
                            })?;
                        FirExprKind::InRange {
                            operation: Self::range_operation(*kind),
                            comparison: self.resolved_type(
                                self.file.expr_span(expression).ok_or_else(|| {
                                    self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                                })?,
                                comparison,
                            )?,
                            value: self.expression(*value)?,
                            start: self.expression(*start)?,
                            end: self.expression(*end)?,
                            negated: *negated,
                        }
                    }
                }
                Expr::Index { array, indices } => {
                    if self.info.resolved_index_get_call(expression).is_some() {
                        self.source_member_operator_call(expression, "get", *array, indices)?
                    } else if self.info.resolved_member(expression).is_some() {
                        self.member_call(expression, *array, indices)?
                    } else {
                        let receiver_ty = self.info.semantic_ty(*array).non_null();
                        let kind = if receiver_ty == Ty::String {
                            FirIndexedAccessKind::String
                        } else if receiver_ty.array_elem().is_some() {
                            FirIndexedAccessKind::Array
                        } else {
                            return Err(self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Index),
                            ));
                        };
                        FirExprKind::IndexedRead {
                            kind,
                            receiver: self.expression(*array)?,
                            indices: indices
                                .iter()
                                .map(|index| self.expression(*index))
                                .collect::<Result<Vec<_>, _>>()?
                                .into_boxed_slice(),
                        }
                    }
                }
                Expr::If {
                    cond,
                    then_branch,
                    else_branch,
                } => {
                    let result_ty = self.expression_type(expression)?;
                    let then_origin = self.expression_origin(*then_branch)?;
                    let then_conversion =
                        self.selected_value_conversion(*then_branch, result_ty, then_origin)?;
                    let (else_branch, else_conversion) = match else_branch {
                        Some(else_branch) => {
                            let else_origin = self.expression_origin(*else_branch)?;
                            (
                                self.expression(*else_branch)?,
                                self.selected_value_conversion(
                                    *else_branch,
                                    result_ty,
                                    else_origin,
                                )?,
                            )
                        }
                        None => {
                            let cause = self.expression_origin(expression)?;
                            let origin = self
                                .origins
                                .synthetic(cause, SyntheticOriginKind::MissingElseUnit);
                            (
                                self.body.add_expr(FirExpr {
                                    origin,
                                    ty: ResolvedTy::new(Ty::Unit)
                                        .expect("Unit is a publishable FIR type"),
                                    kind: FirExprKind::Block {
                                        statements: Box::new([]),
                                        result: None,
                                    },
                                }),
                                None,
                            )
                        }
                    };
                    FirExprKind::Conditional {
                        condition: self.expression(*cond)?,
                        then_branch: self.expression(*then_branch)?,
                        then_conversion,
                        else_branch,
                        else_conversion,
                    }
                }
                Expr::Block { stmts, trailing } => self.block(stmts, *trailing)?,
                Expr::When { subject, arms } => {
                    let has_subject = subject.is_some();
                    let subject = subject
                        .map(|subject| self.expression(subject))
                        .transpose()?;
                    let branches = arms
                        .iter()
                        .map(|arm| {
                            let origin = self.expression_origin(arm.body)?;
                            let conditions = arm
                                .conditions
                                .iter()
                                .map(|condition| {
                                    let checked = self.expression(condition.expression())?;
                                    Ok(if has_subject && !condition.is_predicate() {
                                        FirWhenCondition::SubjectEquals(checked)
                                    } else {
                                        FirWhenCondition::Predicate(checked)
                                    })
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            let guard =
                                arm.guard.map(|guard| self.expression(guard)).transpose()?;
                            Ok(FirWhenBranch {
                                origin,
                                conditions: conditions.into_boxed_slice(),
                                guard,
                                result: self.expression(arm.body)?,
                            })
                        })
                        .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
                    FirExprKind::When {
                        subject,
                        branches: branches.into_boxed_slice(),
                    }
                }
                Expr::Try {
                    body,
                    catches,
                    finally,
                } => {
                    let body = self.expression(*body)?;
                    let mut checked_catches = Vec::with_capacity(catches.len());
                    for catch in catches {
                        let parameter_ty = self.info.resolved_type(&catch.ty).ok_or_else(|| {
                            self.failure(
                                Some(catch.ty.span),
                                BodyCheckFailureKind::UnresolvedTypeSyntax,
                            )
                        })?;
                        self.scopes.push(HashMap::new());
                        self.delegate_scopes.push(HashMap::new());
                        let parameter_ty = self.resolved_type(catch.ty.span, parameter_ty)?;
                        let parameter = self.bind_local(&catch.name, parameter_ty);
                        let checked_body = self.expression(catch.body);
                        self.delegate_scopes.pop();
                        self.scopes.pop();
                        checked_catches.push(FirCatch {
                            origin: self.origins.source(self.source, catch.param_span),
                            parameter,
                            parameter_ty,
                            body: checked_body?,
                        });
                    }
                    FirExprKind::Try {
                        body,
                        catches: checked_catches.into_boxed_slice(),
                        finally: finally
                            .map(|finally| self.expression(finally))
                            .transpose()?,
                    }
                }
                Expr::Call { .. }
                    if self.file.anonymous_object_classes.contains_key(&expression) =>
                {
                    self.anonymous_object(expression)?
                }
                Expr::Call { args, .. }
                    if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::CompilerSynthetic(_))
                    ) =>
                {
                    self.compiler_synthetic_array(expression, args)?
                }
                Expr::Call { args, .. } if self.info.resolved_constructor(expression).is_some() => {
                    self.constructor_call(expression, args)?
                }
                // A SAM CONSTRUCTOR (`I { … }`, `KRunnable { "O" }`): the fun-interface name applied to a
                // function value. It is not a class constructor — it is the same SAM conversion the
                // argument path already builds, applied to that one operand, and its result IS the
                // interface instance.
                Expr::Call { args, .. }
                    if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::SamConstructor { .. })
                    ) =>
                {
                    self.sam_constructor_call(expression, args)?
                }
                Expr::Call { callee, args }
                    if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::ReceiverFnInvoke { .. })
                    ) =>
                {
                    let explicit_receiver = match self.file.expr(*callee) {
                        Expr::Member { receiver, .. } => Some(*receiver),
                        Expr::Name(_) => None,
                        _ => {
                            return Err(self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnsupportedCallShape,
                            ));
                        }
                    };
                    let kind =
                        self.receiver_function_invoke(expression, explicit_receiver, args)?;
                    let ty = self.invoke_result_type(expression, &kind)?;
                    return self.add_expression_with_type(expression, ty, kind);
                }
                Expr::Call { args, .. }
                    if matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::Invoke { .. })
                    ) =>
                {
                    let ExprLowering::Invoke {
                        receiver,
                        params,
                        kind,
                    } = self
                        .info
                        .expr_lowers
                        .get(&expression)
                        .cloned()
                        .expect("invoke lowering was matched")
                    else {
                        unreachable!("invoke guard must retain invoke lowering")
                    };
                    let kind = self.invoke(expression, receiver, args, &params, kind)?;
                    let ty = self.invoke_result_type(expression, &kind)?;
                    return self.add_expression_with_type(expression, ty, kind);
                }
                Expr::Call { callee, args } if matches!(self.file.expr(*callee), Expr::Name(_)) => {
                    self.unqualified_call(expression, args)?
                }
                Expr::Call { callee, args } => match self.file.expr(*callee) {
                    Expr::Member { receiver, .. } => {
                        self.qualified_call(expression, *receiver, args)?
                    }
                    Expr::IntLit(_)
                    | Expr::LongLit(_)
                    | Expr::UIntLit(_)
                    | Expr::ULongLit(_)
                    | Expr::DoubleLit(_)
                    | Expr::FloatLit(_)
                    | Expr::BoolLit(_)
                    | Expr::StringLit(_)
                    | Expr::CharLit(_)
                    | Expr::NullLit
                    | Expr::AnnotationArrayLiteral(_)
                    | Expr::UnsupportedAnnotationArgument(_)
                    | Expr::Name(_)
                    | Expr::NotNull { .. }
                    | Expr::Elvis { .. }
                    | Expr::Template(_)
                    | Expr::Throw { .. }
                    | Expr::Return { .. }
                    | Expr::Is { .. }
                    | Expr::As { .. }
                    | Expr::Unary { .. }
                    | Expr::Binary { .. }
                    | Expr::RangeTo { .. }
                    | Expr::InRange { .. }
                    | Expr::If { .. }
                    | Expr::Block { .. }
                    | Expr::When { .. }
                    | Expr::Try { .. }
                    | Expr::SafeCall { .. }
                    | Expr::Break { .. }
                    | Expr::Continue { .. }
                    | Expr::Lambda { .. }
                    | Expr::IncDec { .. }
                    | Expr::ExtensionAccess { .. }
                    | Expr::Index { .. }
                    | Expr::Call { .. }
                    | Expr::CallableRef { .. } => {
                        return Err(self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Call),
                        ));
                    }
                },
                Expr::Break { label } | Expr::Continue { label } => {
                    let (target_depth, target) =
                        self.loop_target(label.as_deref()).ok_or_else(|| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnsupportedExpression(
                                    super::coverage::expression_form(self.file.expr(expression)),
                                ),
                            )
                        })?;
                    FirExprKind::Jump {
                        kind: if matches!(self.file.expr(expression), Expr::Break { .. }) {
                            FirJumpKind::Break { target_depth }
                        } else {
                            FirJumpKind::Continue { target_depth }
                        },
                        target,
                        value: None,
                    }
                }
                Expr::Member { receiver, .. } => {
                    if let Some(entry) = self.info.resolved_enum_entry(expression) {
                        FirExprKind::EnumEntry {
                            classifier: entry.classifier,
                            ordinal: entry.ordinal,
                            name: entry.name.clone().into_boxed_str(),
                        }
                    } else if let Some(constant) = self.info.resolved_constants.get(&expression) {
                        // The constant payload is the selected value, but Kotlin still evaluates an
                        // ordinary VALUE receiver (`config().VALUE`) for effects before inlining it.
                        // Qualifier/classifier receivers have no entry in this map and remain a plain
                        // constant. Publish the sequencing explicitly in checked FIR so lowering does
                        // not need to rediscover whether the source prefix was a runtime expression.
                        let constant = checked_constant_value(constant);
                        if let Some(receiver) = self.info.resolved_constant_receiver(expression) {
                            let receiver_value = self.expression(receiver)?;
                            let receiver_origin = self.expression_origin(receiver)?;
                            let receiver_statement = self.body.add_statement(FirStatement {
                                origin: receiver_origin,
                                kind: FirStatementKind::Expression(receiver_value),
                            });
                            let value_origin = self.expression_origin(expression)?;
                            let value_ty = self.expression_type(expression)?;
                            let value = self.body.add_expr(FirExpr {
                                origin: value_origin,
                                ty: value_ty,
                                kind: FirExprKind::Constant(constant),
                            });
                            FirExprKind::Block {
                                statements: Box::new([receiver_statement]),
                                result: Some(value),
                            }
                        } else {
                            FirExprKind::Constant(constant)
                        }
                    } else {
                        match self.source_property_read(expression, Some(*receiver))? {
                            Some(property) => property,
                            // A property read the checker resolved as a ZERO-ARG MEMBER CALL rather than
                            // as a property: `list.size` selects `Collection.getSize()`, recorded in
                            // `resolved_calls`. That IS the checker's final decision, so FIR consumes it
                            // instead of demanding a second decision channel — a dependency member with
                            // no Kotlin property metadata never gets one.
                            None => self.zero_argument_member_read(expression, *receiver)?,
                        }
                    }
                }
                Expr::SafeCall { receiver, args, .. } => {
                    crate::trace_compiler!(
                    "fir",
                    "safe call expression={expression:?} receiver={receiver:?} args={} lowering={:?}",
                    args.as_ref().map_or(0, Vec::len),
                    self.info.expr_lowers.get(&expression),
                );
                    let receiver_function = matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::ReceiverFnInvoke { .. })
                    );
                    let safe_property_invoke = match self.info.expr_lowers.get(&expression).cloned()
                    {
                        Some(ExprLowering::SafePropertyInvoke {
                            property,
                            property_ty,
                            params,
                            kind,
                        }) => Some((property, property_ty, params, kind)),
                        _ => None,
                    };
                    let invoke = matches!(
                        self.info.expr_lowers.get(&expression),
                        Some(ExprLowering::Invoke { .. } | ExprLowering::SafePropertyInvoke { .. })
                    );
                    let mut property_guarded_receiver = None;
                    let selector_kind = match args {
                        Some(arguments) if safe_property_invoke.is_some() => {
                            let (property, property_ty, params, kind) = safe_property_invoke
                                .expect("safe property invoke guard retained its selection");
                            let property_kind = self
                                .source_property_read_selected(
                                    expression,
                                    Some(*receiver),
                                    Some(*property),
                                    None,
                                )?
                                .ok_or_else(|| {
                                    self.failure(
                                        self.file.expr_span(expression),
                                        BodyCheckFailureKind::MissingStablePropertyTarget,
                                    )
                                })?;
                            property_guarded_receiver =
                                Self::safe_selector_receiver(&property_kind);
                            let origin = self.expression_origin(expression)?;
                            let property_ty = self.resolved_type(
                                self.file.expr_span(expression).ok_or_else(|| {
                                    self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                                })?,
                                property_ty,
                            )?;
                            let property_value = self.body.add_expr(FirExpr {
                                origin,
                                ty: property_ty,
                                kind: property_kind,
                            });
                            self.invoke_on_value(
                                expression,
                                property_value,
                                arguments,
                                &params,
                                kind,
                            )?
                        }
                        Some(arguments) if receiver_function => {
                            self.receiver_function_invoke(expression, Some(*receiver), arguments)?
                        }
                        Some(arguments) if invoke => {
                            let ExprLowering::Invoke {
                                receiver,
                                params,
                                kind,
                            } = self
                                .info
                                .expr_lowers
                                .get(&expression)
                                .cloned()
                                .expect("safe invoke lowering was matched")
                            else {
                                unreachable!("safe invoke guard must retain invoke lowering")
                            };
                            self.invoke(expression, receiver, arguments, &params, kind)?
                        }
                        Some(arguments) if self.info.resolved_constructor(expression).is_some() => {
                            self.constructor_call(expression, arguments)?
                        }
                        Some(arguments) => self.qualified_call(expression, *receiver, arguments)?,
                        None => self
                            .source_property_read(expression, Some(*receiver))?
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.expr_span(expression),
                                    BodyCheckFailureKind::UnsupportedExpression(
                                        ExpressionForm::SafeCall,
                                    ),
                                )
                            })?,
                    };
                    crate::trace_compiler!(
                        "fir",
                        "safe selector expression={expression:?} kind={selector_kind:?}",
                    );
                    let guarded_receiver = property_guarded_receiver
                        .or_else(|| {
                            if receiver_function {
                                Self::receiver_function_argument(&selector_kind)
                            } else {
                                Self::safe_selector_receiver(&selector_kind)
                            }
                        })
                        .ok_or_else(|| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnsupportedExpression(
                                    ExpressionForm::SafeCall,
                                ),
                            )
                        })?;
                    let span = self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?;
                    let selector_ty = self.safe_selector_type(expression).ok_or_else(|| {
                        self.failure(
                            Some(span),
                            BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::SafeCall),
                        )
                    });
                    let selector_ty = if receiver_function || invoke {
                        self.invoke_result_type(expression, &selector_kind)?.get()
                    } else {
                        selector_ty?
                    };
                    let selector_origin = self.expression_origin(expression)?;
                    let selector_ty = self.resolved_type(span, selector_ty)?;
                    let selector = self.body.add_expr(FirExpr {
                        origin: selector_origin,
                        ty: selector_ty,
                        kind: selector_kind,
                    });
                    let kind = FirExprKind::SafeCall {
                        receiver: guarded_receiver,
                        selector,
                    };
                    let ty = self.resolved_type(span, Ty::nullable(selector_ty.get()))?;
                    return self.add_expression_with_type(expression, ty, kind);
                }
                Expr::Lambda { params, body } => self.lambda(expression, params, *body)?,
                Expr::CallableRef { receiver, .. } => {
                    self.callable_reference(expression, *receiver)?
                }
                Expr::ExtensionAccess { receiver, callable } => {
                    self.extension_function_binding(expression, *receiver, *callable)?
                }
            }
        };
        self.add_expression(expression, kind)
    }

    fn block(
        &mut self,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        self.scopes.push(HashMap::new());
        self.delegate_scopes.push(HashMap::new());
        self.local_callable_scopes.push(HashMap::new());
        let result = self.block_in_current_scope(statements, trailing);
        self.scopes.pop();
        self.delegate_scopes.pop();
        self.local_callable_scopes.pop();
        result
    }

    /// Build a block after its owner has opened the lexical scope. Do-while owns that scope because
    /// its body declarations remain visible while the trailing condition is checked.
    fn block_in_current_scope(
        &mut self,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        for statement in statements {
            if matches!(self.file.stmt(*statement), Stmt::LocalFun(_)) {
                let callable = self.body.allocate_local_callable();
                self.local_callable_scopes
                    .last_mut()
                    .expect("a FIR block owns a local callable scope")
                    .insert(*statement, callable);
            }
        }
        let checked = statements
            .iter()
            // A `contract { … }` block is not executable code: it is a compile-time declaration whose
            // lambda uses the `ContractBuilder` DSL, and kotlinc emits no bytecode for it. The checker
            // marks it `StmtLowering::Erased` and deliberately does not type its body, so FIR must
            // drop it rather than try to build a call from an unchecked DSL shape.
            .filter(|statement| {
                !matches!(
                    self.info.stmt_lowers.get(statement),
                    Some(StmtLowering::Erased)
                )
            })
            .map(|statement| self.statement(*statement))
            .collect::<Result<Vec<_>, _>>();
        let result = checked.as_ref().map_or(Ok(None), |_| {
            trailing
                .map(|trailing| self.expression(trailing))
                .transpose()
        });
        Ok(FirExprKind::Block {
            statements: checked?.into_boxed_slice(),
            result: result?,
        })
    }

    /// The parser represents `receiver.name op= rhs` as a write whose value contains the matching
    /// read and deliberately reuses the same receiver expression identity. Kotlin evaluates that
    /// receiver once, so checked FIR binds the shared identity before publishing either access.
    fn is_compound_member_assignment(&self, receiver: ExprId, name: &str, value: ExprId) -> bool {
        let Expr::Binary { lhs, .. } = self.file.expr(value) else {
            return false;
        };
        matches!(
            self.file.expr(*lhs),
            Expr::Member {
                receiver: read_receiver,
                name: read_name,
            } if *read_receiver == receiver && read_name == name
        )
    }

    fn statement(&mut self, statement: StmtId) -> Result<FirStatementId, BodyCheckFailure> {
        let origin = self.statement_origin(statement)?;
        if let Some(StmtLowering::PlusAssign(target)) =
            self.info.stmt_lowers.get(&statement).cloned()
        {
            return self.compound_assignment_statement(statement, target, origin);
        }
        let custom_range_loop = match self.file.stmt(statement) {
            Stmt::For {
                name,
                range,
                body,
                label,
            } => self
                .info
                .for_range_iterator_protocol(statement)
                .cloned()
                .map(|plan| (name.clone(), range.clone(), *body, label.clone(), plan)),
            Stmt::Local { .. }
            | Stmt::LocalLateinit { .. }
            | Stmt::LocalDelegate { .. }
            | Stmt::Destructure { .. }
            | Stmt::Assign { .. }
            | Stmt::IncDec { .. }
            | Stmt::AssignMember { .. }
            | Stmt::AssignIndex { .. }
            | Stmt::CompoundAssign { .. }
            | Stmt::Return(..)
            | Stmt::While { .. }
            | Stmt::DoWhile { .. }
            | Stmt::ForEach { .. }
            | Stmt::Break(..)
            | Stmt::Continue(..)
            | Stmt::Expr(..)
            | Stmt::LocalFun(..)
            | Stmt::LocalTypeAlias(..)
            | Stmt::LocalClass(..) => None,
        };
        if let Some((name, range, source_body, label, plan)) = custom_range_loop {
            let target = self.body.add_control_target(FirControlTarget {
                origin,
                kind: FirControlTargetKind::Loop,
            });
            let convention = match range.kind {
                RangeKind::Through => "rangeTo",
                RangeKind::OpenEnd => "rangeUntil",
                RangeKind::Until => "until",
                RangeKind::DownTo => "downTo",
            };
            let call = self.source_member_statement_operator_call(
                statement,
                convention,
                range.start,
                &[range.end],
            )?;
            let span = self
                .file
                .stmt_spans
                .get(statement.0 as usize)
                .copied()
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
            let range_ty = self.resolved_type(span, plan.range.ret())?;
            let iterable = self.body.add_expr(FirExpr {
                origin: self
                    .origins
                    .synthetic(origin, SyntheticOriginKind::GeneratedControlFlow),
                ty: range_ty,
                kind: FirExprKind::Call(call),
            });
            let variable_ty = self.resolved_type(span, plan.protocol.elem_ty)?;
            let variable = self.allocate_local();
            let body = self.checked_loop_body(
                target,
                &label,
                Some((
                    name.as_str(),
                    LocalBinding {
                        value: variable,
                        ty: variable_ty,
                    },
                )),
                source_body,
            )?;
            let header = self.iterator_loop_header_from_protocol(
                statement,
                variable,
                variable_ty,
                iterable,
                &plan.protocol,
            )?;
            return Ok(self.body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Loop {
                    target,
                    header,
                    body,
                },
            }));
        }
        let kind = match self.file.stmt(statement) {
            Stmt::Local {
                is_var,
                name,
                ty,
                init,
            } => {
                let initializer = self.expression(*init)?;
                // An unnamed local is an evaluation statement, not storage. Kotlin guarantees the
                // initializer runs, but `_` introduces no readable binding and therefore has no
                // value slot for common lowering to store into. This is distinct from an ignored
                // destructuring entry: the latter shares one already-materialized initializer.
                if name == "_" {
                    return Ok(self.body.add_statement(FirStatement {
                        origin,
                        kind: FirStatementKind::Expression(initializer),
                    }));
                }
                let local_ty = match ty {
                    Some(ty) => self.info.resolved_type(ty).ok_or_else(|| {
                        self.failure(Some(ty.span), BodyCheckFailureKind::UnresolvedTypeSyntax)
                    })?,
                    None => self
                        .info
                        .local_decl_types
                        .get(&statement)
                        .copied()
                        .unwrap_or_else(|| self.info.semantic_ty(*init)),
                };
                let type_span = ty
                    .as_ref()
                    .map(|ty| ty.span)
                    .or_else(|| self.file.expr_span(*init));
                let Some(type_span) = type_span else {
                    return Err(self.failure(None, BodyCheckFailureKind::MissingSourceSpan));
                };
                let local_ty = self.resolved_type(type_span, local_ty)?;
                let conversion = self.selected_value_conversion(*init, local_ty, origin)?;
                FirStatementKind::Local {
                    target: self.bind_local(name, local_ty),
                    ty: local_ty,
                    mutable: *is_var,
                    lateinit: false,
                    initializer: Some(initializer),
                    conversion,
                }
            }
            Stmt::LocalLateinit { name, ty } => {
                let local_ty = self.info.resolved_type(ty).ok_or_else(|| {
                    self.failure(Some(ty.span), BodyCheckFailureKind::UnresolvedTypeSyntax)
                })?;
                let local_ty = self.resolved_type(ty.span, local_ty)?;
                FirStatementKind::Local {
                    target: self.bind_local(name, local_ty),
                    ty: local_ty,
                    mutable: true,
                    lateinit: true,
                    initializer: None,
                    conversion: None,
                }
            }
            Stmt::Assign { name, value } => {
                let write = if matches!(
                    self.info.stmt_lowers.get(&statement),
                    Some(StmtLowering::BackingFieldWrite)
                ) {
                    FirExprKind::BackingFieldWrite {
                        target: self.enclosing_property_for_statement(statement)?,
                        value: self.expression(*value)?,
                        conversion: None,
                    }
                } else if let Some((depth, delegate)) = self.delegated_binding(name) {
                    self.delegated_write(statement, depth, delegate, *value)?
                } else if let Some(target) = self.local(name) {
                    FirExprKind::ValueWrite {
                        target,
                        value: self.expression(*value)?,
                        conversion: None,
                    }
                } else if let Some((enclosing_depth, binding)) =
                    self.outer_values.get(name).copied()
                {
                    self.body.add_capture(FirCapture {
                        origin,
                        enclosing_depth,
                        source: binding.value,
                        ty: binding.ty,
                        shared_cell: true,
                    });
                    FirExprKind::CapturedValueWrite {
                        enclosing_depth,
                        source: binding.value,
                        value: self.expression(*value)?,
                        conversion: None,
                    }
                } else if let Some(binding) = self.class_values.get(name).copied() {
                    if !binding.shared_cell {
                        return Err(self.failure(
                            self.file.stmt_spans.get(statement.0 as usize).copied(),
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::Assign),
                        ));
                    }
                    let value = self.expression(*value)?;
                    self.class_storage_shared_write_kind(binding, origin, value, None)?
                } else if let Some(StmtLowering::DeferredPropertyWrite {
                    enum_entry_property,
                    ..
                }) = self.info.stmt_lowers.get(&statement)
                {
                    // A deferred `val` assigned from an `init` block or a constructor body. The
                    // checker already committed the owner and type; the write is a direct backing
                    // field store, exactly as an initializer would have been.
                    let target = enum_entry_property
                        .and_then(|sibling| self.enum_entry_property(sibling))
                        .or_else(|| self.classifier_property_named(name))
                        .ok_or_else(|| {
                            self.failure(
                                self.file.stmt_spans.get(statement.0 as usize).copied(),
                                BodyCheckFailureKind::MissingStablePropertyTarget,
                            )
                        })?;
                    FirExprKind::BackingFieldWrite {
                        target,
                        value: self.expression(*value)?,
                        conversion: None,
                    }
                } else if let Some(property) =
                    self.source_property_write(statement, None, *value)?
                {
                    property
                } else {
                    crate::trace_compiler!(
                        "fir",
                        "assignment target is unknown: name={name:?} stmt_lowering={:?}",
                        self.info.stmt_lowers.get(&statement),
                    );
                    return Err(self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnknownLocal,
                    ));
                };
                let expression = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                    kind: write,
                });
                FirStatementKind::Expression(expression)
            }
            Stmt::AssignMember {
                receiver,
                name,
                value,
                safe,
            } => {
                // `super.p = v` selects the SUPER setter — an ordinary super call whose single
                // argument is the assigned value. The read side already routes through
                // `resolved_super_calls`; this is its write counterpart.
                let super_target = match self.info.stmt_lowers.get(&statement).cloned() {
                    Some(StmtLowering::SuperPropertyWrite { target }) => Some(target),
                    _ => None,
                };
                let receiver_binding = if super_target.is_none()
                    && self.is_compound_member_assignment(*receiver, name, *value)
                {
                    let initializer = self.expression(*receiver)?;
                    let ty = self.expression_type(*receiver)?;
                    let target = self.allocate_local();
                    let declaration = self.body.add_statement(FirStatement {
                        origin,
                        kind: FirStatementKind::Local {
                            target,
                            ty,
                            mutable: false,
                            lateinit: false,
                            initializer: Some(initializer),
                            conversion: None,
                        },
                    });
                    let replacement = self.body.add_expr(FirExpr {
                        origin,
                        ty,
                        kind: FirExprKind::ValueRead(target),
                    });
                    self.expression_substitutions.insert(*receiver, replacement);
                    Some(declaration)
                } else {
                    None
                };
                let write = if let Some(target) = super_target {
                    let span = self.file.stmt_spans.get(statement.0 as usize).copied();
                    self.selected_super_property_write(span, origin, *value, &target)
                } else {
                    self.source_property_write(statement, Some(*receiver), *value)
                        .and_then(|write| {
                            write.ok_or_else(|| {
                                self.failure(
                                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                                    BodyCheckFailureKind::UnsupportedStatement(
                                        StatementForm::AssignMember,
                                    ),
                                )
                            })
                        })
                };
                self.expression_substitutions.remove(receiver);
                let write = write?;
                let selector = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                    kind: write,
                });
                let expression = if *safe {
                    self.body.add_expr(FirExpr {
                        origin,
                        ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                        kind: FirExprKind::SafeCall {
                            receiver: Self::safe_selector_receiver(
                                &self.body.expr(selector).expect("safe selector exists").kind,
                            )
                            .ok_or_else(|| {
                                self.failure(
                                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                                    BodyCheckFailureKind::UnsupportedStatement(
                                        StatementForm::AssignMember,
                                    ),
                                )
                            })?,
                            selector,
                        },
                    })
                } else {
                    selector
                };
                let expression = if let Some(declaration) = receiver_binding {
                    self.body.add_expr(FirExpr {
                        origin,
                        ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                        kind: FirExprKind::Block {
                            statements: vec![declaration].into_boxed_slice(),
                            result: Some(expression),
                        },
                    })
                } else {
                    expression
                };
                FirStatementKind::Expression(expression)
            }
            Stmt::IncDec { name, dec, .. } => {
                if let Some((depth, delegate)) = self.delegated_binding(name) {
                    let write =
                        self.delegated_inc_dec_statement(statement, *dec, depth, delegate, origin)?;
                    return Ok(self.body.add_statement(FirStatement {
                        origin,
                        kind: FirStatementKind::Expression(write),
                    }));
                }
                let target = self.local(name);
                let captured = if target.is_none() {
                    self.outer_values.get(name).copied()
                } else {
                    None
                };
                let class_storage = if target.is_none() && captured.is_none() {
                    self.class_values.get(name).copied()
                } else {
                    None
                };
                // `c++` where `c` is a MEMBER property of the enclosing classifier is neither a local
                // nor a capture; the checker resolved it through the implicit receiver and recorded
                // the write target. In STATEMENT position the updated value is discarded, so this is
                // exactly read → operator → write with no prefix/postfix distinction to preserve.
                if target.is_none() && captured.is_none() && class_storage.is_none() {
                    // `field++` inside a property accessor. The checker records the backing-field
                    // write; read and write both address the enclosing property's storage directly,
                    // with no getter/setter in between.
                    if matches!(
                        self.info.stmt_lowers.get(&statement),
                        Some(StmtLowering::BackingFieldWrite)
                    ) {
                        let property = self.enclosing_property_for_statement(statement)?;
                        let span = self
                            .file
                            .stmt_spans
                            .get(statement.0 as usize)
                            .copied()
                            .ok_or_else(|| {
                                self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                            })?;
                        let resolution = self
                            .info
                            .resolved_inc_dec
                            .get(&IncDecSite::Statement(statement))
                            .copied()
                            .ok_or_else(|| {
                                self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::UnsupportedStatement(
                                        StatementForm::IncDec,
                                    ),
                                )
                            })?;
                        let read = self.body.add_expr(FirExpr {
                            origin,
                            ty: self.resolved_type(span, resolution.receiver_ty)?,
                            kind: FirExprKind::BackingFieldRead { target: property },
                        });
                        let convention = if *dec { "dec" } else { "inc" };
                        let updated_kind = if self
                            .info
                            .resolved_stmt_operator_call(statement, convention)
                            .is_some()
                        {
                            self.zero_arg_statement_operator_call_on_value(
                                statement, convention, read,
                            )?
                        } else {
                            FirExprKind::Unary {
                                operation: if *dec {
                                    FirUnaryOperation::Decrement
                                } else {
                                    FirUnaryOperation::Increment
                                },
                                operand: read,
                            }
                        };
                        let updated = self.body.add_expr(FirExpr {
                            origin,
                            ty: self.resolved_type(span, resolution.updated_ty)?,
                            kind: updated_kind,
                        });
                        let write = self.body.add_expr(FirExpr {
                            origin,
                            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                            kind: FirExprKind::BackingFieldWrite {
                                target: property,
                                value: updated,
                                conversion: None,
                            },
                        });
                        return Ok(self.body.add_statement(FirStatement {
                            origin,
                            kind: FirStatementKind::Expression(write),
                        }));
                    }
                    if let Some(kind) = self.implicit_property_inc_dec(statement, *dec)? {
                        let write = self.body.add_expr(FirExpr {
                            origin,
                            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                            kind,
                        });
                        return Ok(self.body.add_statement(FirStatement {
                            origin,
                            kind: FirStatementKind::Expression(write),
                        }));
                    }
                    return Err(self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnknownLocal,
                    ));
                }
                let convention = if *dec { "dec" } else { "inc" };
                let selected_operator = self
                    .info
                    .resolved_stmt_operator_call(statement, convention)
                    .is_some();
                let resolution = self
                    .info
                    .resolved_inc_dec
                    .get(&IncDecSite::Statement(statement))
                    .ok_or_else(|| {
                        self.failure(
                            self.file.stmt_spans.get(statement.0 as usize).copied(),
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::IncDec),
                        )
                    })?;
                let span = self
                    .file
                    .stmt_spans
                    .get(statement.0 as usize)
                    .copied()
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
                if let Some((enclosing_depth, binding)) = captured {
                    self.body.add_capture(FirCapture {
                        origin,
                        enclosing_depth,
                        source: binding.value,
                        ty: binding.ty,
                        shared_cell: true,
                    });
                }
                let read_kind = match (target, captured, class_storage) {
                    (Some(target), _, _) => FirExprKind::ValueRead(target),
                    (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueRead {
                        enclosing_depth,
                        source: binding.value,
                    },
                    (None, None, Some(binding)) => self.class_storage_read_kind(binding, origin)?,
                    (None, None, None) => unreachable!("increment target was checked above"),
                };
                let read = self.body.add_expr(FirExpr {
                    origin,
                    ty: self.resolved_type(span, resolution.receiver_ty)?,
                    kind: read_kind,
                });
                let updated_kind = if selected_operator {
                    self.zero_arg_statement_operator_call_on_value(statement, convention, read)?
                } else {
                    FirExprKind::Unary {
                        operation: if *dec {
                            FirUnaryOperation::Decrement
                        } else {
                            FirUnaryOperation::Increment
                        },
                        operand: read,
                    }
                };
                let updated = self.body.add_expr(FirExpr {
                    origin,
                    ty: self.resolved_type(span, resolution.updated_ty)?,
                    kind: updated_kind,
                });
                let write_kind = match (target, captured, class_storage) {
                    (Some(target), _, _) => FirExprKind::ValueWrite {
                        target,
                        value: updated,
                        conversion: None,
                    },
                    (None, Some((enclosing_depth, binding)), _) => {
                        FirExprKind::CapturedValueWrite {
                            enclosing_depth,
                            source: binding.value,
                            value: updated,
                            conversion: None,
                        }
                    }
                    (None, None, Some(binding)) => {
                        self.class_storage_shared_write_kind(binding, origin, updated, None)?
                    }
                    (None, None, None) => unreachable!("increment target was checked above"),
                };
                let write = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                    kind: write_kind,
                });
                FirStatementKind::Expression(write)
            }
            Stmt::AssignIndex {
                array,
                indices,
                value,
            } => {
                let receiver_ty = self.info.semantic_ty(*array).non_null();
                let selected_convention = ["set", "put"].into_iter().find(|convention| {
                    self.info
                        .resolved_stmt_operator_call(statement, convention)
                        .is_some()
                });
                let expression = if let Some(convention) = selected_convention {
                    let mut operands = indices.clone();
                    operands.push(*value);
                    let call = self.source_member_statement_operator_call(
                        statement, convention, *array, &operands,
                    )?;
                    let selected_result = self
                        .info
                        .resolved_stmt_operator_call(statement, convention)
                        .map(ResolvedCall::ret)
                        .ok_or_else(|| {
                            self.failure(
                                self.file.stmt_spans.get(statement.0 as usize).copied(),
                                BodyCheckFailureKind::UnsupportedCallShape,
                            )
                        })?;
                    let span = self
                        .file
                        .stmt_spans
                        .get(statement.0 as usize)
                        .copied()
                        .ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?;
                    let call_ty = self.resolved_type(span, selected_result)?;
                    let call = self.body.add_expr(FirExpr {
                        origin,
                        ty: call_ty,
                        kind: FirExprKind::Call(call),
                    });
                    let unit = ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type");
                    if call_ty == unit {
                        call
                    } else {
                        let conversion = self
                            .selected_type_conversion(call_ty, unit, origin)
                            .expect("an indexed assignment discards its selected set result");
                        self.body.add_expr(FirExpr {
                            origin,
                            ty: unit,
                            kind: FirExprKind::ImplicitConversion {
                                value: call,
                                conversion,
                            },
                        })
                    }
                } else {
                    if indices.len() != 1 || receiver_ty.array_elem().is_none() {
                        return Err(self.failure(
                            self.file.stmt_spans.get(statement.0 as usize).copied(),
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::AssignIndex),
                        ));
                    }
                    let receiver = self.expression(*array)?;
                    let indices = indices
                        .iter()
                        .map(|index| self.expression(*index))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_boxed_slice();
                    let element_type = self.resolved_type(
                        self.file
                            .stmt_spans
                            .get(statement.0 as usize)
                            .copied()
                            .ok_or_else(|| {
                                self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                            })?,
                        receiver_ty
                            .array_elem()
                            .expect("checked array element type"),
                    )?;
                    let conversion =
                        self.selected_value_conversion(*value, element_type, origin)?;
                    let value = self.expression(*value)?;
                    self.body.add_expr(FirExpr {
                        origin,
                        ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                        kind: FirExprKind::IndexedWrite {
                            receiver,
                            indices,
                            value,
                            conversion,
                        },
                    })
                };
                FirStatementKind::Expression(expression)
            }
            Stmt::Return(value, label) => {
                let target = self
                    .info
                    .stmt_return_targets
                    .get(&statement)
                    .copied()
                    .or_else(|| label.is_none().then(|| self.default_return_target()));
                let Some(target_depth) = self.return_target_depth(target.as_ref()) else {
                    return Err(self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnsupportedStatement(StatementForm::Return),
                    ));
                };
                let value = value
                    .map(|value| self.return_value(value, target.as_ref()))
                    .transpose()?;
                let expression = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Nothing).expect("Nothing is a publishable FIR type"),
                    kind: FirExprKind::Jump {
                        kind: FirJumpKind::Return { target_depth },
                        target: self.return_target,
                        value,
                    },
                });
                FirStatementKind::Expression(expression)
            }
            Stmt::Break(label) | Stmt::Continue(label) => {
                let (target_depth, target) =
                    self.loop_target(label.as_deref()).ok_or_else(|| {
                        self.failure(
                            self.file.stmt_spans.get(statement.0 as usize).copied(),
                            BodyCheckFailureKind::UnsupportedStatement(
                                super::coverage::statement_form(self.file.stmt(statement)),
                            ),
                        )
                    })?;
                let expression = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Nothing).expect("Nothing is a publishable FIR type"),
                    kind: FirExprKind::Jump {
                        kind: if matches!(self.file.stmt(statement), Stmt::Break(_)) {
                            FirJumpKind::Break { target_depth }
                        } else {
                            FirJumpKind::Continue { target_depth }
                        },
                        target,
                        value: None,
                    },
                });
                FirStatementKind::Expression(expression)
            }
            Stmt::While { cond, body, label } => {
                let target = self.body.add_control_target(FirControlTarget {
                    origin,
                    kind: FirControlTargetKind::Loop,
                });
                let condition = self.expression(*cond)?;
                let body = self.checked_loop_body(target, label, None, *body)?;
                FirStatementKind::Loop {
                    target,
                    header: FirLoopHeader::While { condition },
                    body,
                }
            }
            Stmt::DoWhile { body, cond, label } => {
                let target = self.body.add_control_target(FirControlTarget {
                    origin,
                    kind: FirControlTargetKind::Loop,
                });
                let (body, condition) = self.checked_do_while(target, label, *body, *cond)?;
                FirStatementKind::Loop {
                    target,
                    header: FirLoopHeader::DoWhile { condition },
                    body,
                }
            }
            Stmt::For {
                name,
                range,
                body,
                label,
            } => {
                let target = self.body.add_control_target(FirControlTarget {
                    origin,
                    kind: FirControlTargetKind::Loop,
                });
                // A counted range commits each flexible platform operand to its non-null lower
                // bound. The resolver made the same choice when selecting the built-in range
                // operation; FIR must publish that checked decision instead of rejecting `Int!`
                // produced by a Java collection accessor.
                let start_ty = self.info.ty(range.start).range_operand_bound();
                let end_ty = self.info.ty(range.end).range_operand_bound();
                let variable_ty = if start_ty == Ty::Nothing {
                    end_ty.range_counter_type()
                } else if end_ty == Ty::Nothing {
                    start_ty.range_counter_type()
                } else {
                    Ty::range_counter_type_for(start_ty, end_ty)
                }
                .ok_or_else(|| {
                    self.failure(
                        self.file.stmt_spans.get(statement.0 as usize).copied(),
                        BodyCheckFailureKind::UnsupportedStatement(StatementForm::For),
                    )
                })?;
                let counter = match variable_ty {
                    Ty::Int => crate::fir::FirRangeCounterKind::Int,
                    Ty::Long => crate::fir::FirRangeCounterKind::Long,
                    Ty::Char => crate::fir::FirRangeCounterKind::Char,
                    Ty::UInt => crate::fir::FirRangeCounterKind::UInt,
                    Ty::ULong => crate::fir::FirRangeCounterKind::ULong,
                    _ => {
                        return Err(self.failure(
                            self.file.stmt_spans.get(statement.0 as usize).copied(),
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::For),
                        ));
                    }
                };
                let variable_ty = self.resolved_type(
                    self.file
                        .stmt_spans
                        .get(statement.0 as usize)
                        .copied()
                        .ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                    variable_ty,
                )?;
                let start = self.expression(range.start)?;
                let end = self.expression(range.end)?;
                let variable = self.allocate_local();
                let body = self.checked_loop_body(
                    target,
                    label,
                    Some((
                        name.as_str(),
                        LocalBinding {
                            value: variable,
                            ty: variable_ty,
                        },
                    )),
                    *body,
                )?;
                FirStatementKind::Loop {
                    target,
                    header: FirLoopHeader::Range {
                        variable,
                        counter,
                        operation: Self::range_operation(range.kind),
                        start,
                        end,
                    },
                    body,
                }
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                label,
            } => {
                let target = self.body.add_control_target(FirControlTarget {
                    origin,
                    kind: FirControlTargetKind::Loop,
                });
                let iteration_ty = self.info.semantic_ty(*iterable).platform_lower_bound();
                let element_ty = iteration_ty
                    .array_elem()
                    .or((iteration_ty == Ty::String).then_some(Ty::Char))
                    .or_else(|| {
                        self.info
                            .iterator_protocol(*iterable)
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
                        .ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                    element_ty,
                )?;
                let variable = self.allocate_local();
                let body = self.checked_loop_body(
                    target,
                    label,
                    Some((
                        name.as_str(),
                        LocalBinding {
                            value: variable,
                            ty: element_ty,
                        },
                    )),
                    *body,
                )?;
                let iterable_expression = self.expression(*iterable)?;
                let header = if iteration_ty.array_elem().is_some() || iteration_ty == Ty::String {
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
                } else {
                    self.iterator_loop_header(
                        statement,
                        *iterable,
                        variable,
                        element_ty,
                        iterable_expression,
                    )?
                };
                FirStatementKind::Loop {
                    target,
                    header,
                    body,
                }
            }
            Stmt::Expr(expression) => FirStatementKind::Expression(self.expression(*expression)?),
            Stmt::Destructure { entries, init } => {
                return self.destructure_statement(statement, entries, *init, origin);
            }
            Stmt::LocalFun(function) => {
                return self.local_function_statement(statement, function, origin);
            }
            Stmt::LocalDelegate {
                is_var,
                name,
                ty,
                delegate,
            } => {
                return self.local_delegate_statement(
                    statement,
                    *is_var,
                    name,
                    ty.as_ref(),
                    *delegate,
                    origin,
                );
            }
            Stmt::LocalTypeAlias(_) => FirStatementKind::LocalTypeAlias,
            Stmt::LocalClass(class) => {
                return self.local_class_statement(statement, class, origin);
            }
            Stmt::CompoundAssign { .. } => {
                return Err(self.failure(
                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                    BodyCheckFailureKind::UnsupportedStatement(super::coverage::statement_form(
                        self.file.stmt(statement),
                    )),
                ));
            }
        };
        Ok(self.body.add_statement(FirStatement { origin, kind }))
    }
}

impl BodyFirChecker<'_> {
    fn default_return_target(&self) -> ReturnTarget {
        self.lambda_return_source
            .map_or(ReturnTarget::Function, ReturnTarget::Lambda)
    }

    fn return_value(
        &mut self,
        source: ExprId,
        target: Option<&ReturnTarget>,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let target_type = match target {
            Some(ReturnTarget::Lambda(lambda)) if Some(*lambda) == self.lambda_return_source => {
                self.body.result_type()
            }
            Some(ReturnTarget::Function) if self.lambda_return_source.is_some() => self
                .index
                .signature(DeclarationId::from_raw(self.body.owner().raw()))
                .map(|signature| signature.result),
            Some(ReturnTarget::Function) => self
                .body
                .result_type()
                .or_else(|| {
                    self.index
                        .signature(DeclarationId::from_raw(self.body.owner().raw()))
                        .map(|signature| signature.result)
                })
                .or_else(|| ResolvedTy::new(self.info.semantic_ty(source)).ok()),
            Some(ReturnTarget::Lambda(lambda)) => {
                let Ty::Fun(signature) = self.info.semantic_ty(*lambda).non_null() else {
                    return Err(self.failure(
                        self.file.expr_span(source),
                        BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
                    ));
                };
                ResolvedTy::new(signature.ret).ok()
            }
            None => ResolvedTy::new(self.info.semantic_ty(source)).ok(),
        }
        .ok_or_else(|| {
            self.failure(
                self.file.expr_span(source),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
            )
        })?;
        let cause = self.expression_origin(source)?;
        let conversion = self.selected_value_conversion(source, target_type, cause)?;
        let value = self.expression(source)?;
        let Some(conversion) = conversion else {
            return Ok(value);
        };
        Ok(self.body.add_expr(FirExpr {
            origin: cause,
            ty: target_type,
            kind: FirExprKind::ImplicitConversion { value, conversion },
        }))
    }

    /// Whether a checked `return` leaves the body this checker is constructing.
    ///
    /// A declaration body owns `ReturnTarget::Function`. A lambda or anonymous-function body owns
    /// `ReturnTarget::Lambda` of its own expression: `fun (x: T): R { return e }` returns from the
    /// anonymous function, and `return@label` returns from the labeled lambda. Any other lambda
    /// target is a NON-LOCAL return through an enclosing inline frame, which is a separate checked
    /// form.
    fn return_target_depth(&self, target: Option<&ReturnTarget>) -> Option<u32> {
        match target {
            Some(ReturnTarget::Function) => Some(self.function_return_depth),
            Some(ReturnTarget::Lambda(source)) if Some(*source) == self.lambda_return_source => {
                Some(0)
            }
            Some(ReturnTarget::Lambda(source)) => {
                self.outer_lambda_return_depths.get(source).copied()
            }
            None => None,
        }
    }
}
