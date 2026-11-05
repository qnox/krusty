//! One top-level function's checked body as kotlinc's IR: its declaration from the parser's unit
//! and the index, its statements and expressions from checked FIR.

use krusty_plugin_ir::{
    BodyIr, CallIr, CallableIdentity, CallableKind, Callee, Constant, ExpressionIr, ExpressionKind,
    FunctionIr, ParameterIr, StatementIr, ValueId, VariableIr, Visibility,
};

use crate::ast::{AnnotationRef, File, FunBody, FunDecl, Param};
use crate::compiler::ObservedBody;
use crate::diag::Span;
use crate::fir::{
    DeclarationId, FirCall, FirCallArgument, FirCallTarget, FirConstant, FirConversion,
    FirConversionKind, FirExprId, FirExprKind, FirJumpKind, FirStatementId, FirStatementKind,
    FirTypeParameterRef, LocalValueId, Origin,
};
use crate::fx_hash::HashMap;
use crate::libraries::ExternalCallableKind;
use crate::types::{SemanticCallableOwner, Ty, TyKind, TypeName};

use super::call_spans::CallSpans;
use super::types::{class_name, identity, package_name, TypeParameters};
use super::Refusal;
use crate::diag::kotlinc_offsets::Utf16Offsets;

/// The function `declaration` whose checked body `observed` holds.
pub(super) fn encode_function(
    observed: &ObservedBody<'_>,
    declaration: DeclarationId,
    offsets: &Utf16Offsets,
    calls: &CallSpans,
    positions: &HashMap<DeclarationId, (u32, u32)>,
) -> Result<FunctionIr, Refusal> {
    let index = observed.index;
    let name = index
        .declaration_name(declaration)
        .unwrap_or("")
        .to_string();
    let refuse = |span: Span, what: String| Refusal {
        source: observed.source,
        span,
        what,
    };
    let Some(syntax) = observed.declarations.function(observed.file, declaration) else {
        return Err(refuse(Span::new(0, 0), format!("the function `{name}`")));
    };
    if syntax.receiver.is_some() || syntax.context_count != 0 {
        return Err(refuse(syntax.span, format!("the receivers of `{name}`")));
    }
    if !syntax.type_params.is_empty() {
        return Err(refuse(
            syntax.span,
            format!("the type parameters of `{name}`"),
        ));
    }
    if !syntax.annotations.is_empty() {
        return Err(refuse(syntax.span, format!("the annotations of `{name}`")));
    }
    if let Some(modifier) = modifier(syntax) {
        return Err(refuse(
            syntax.span,
            format!("the `{modifier}` modifier of `{name}`"),
        ));
    }
    if let Some(annotation) = type_annotation(observed.file, syntax.span) {
        return Err(refuse(
            Span::new(annotation.at, annotation.span.hi),
            format!("an annotated type in `{name}`"),
        ));
    }
    let visibility = match index
        .declaration_header(declaration)
        .map(|header| header.visibility)
    {
        Some(crate::types::Visibility::Public) => Visibility::Public,
        Some(crate::types::Visibility::Internal) => Visibility::Internal,
        Some(crate::types::Visibility::Private) => Visibility::Private,
        other => {
            return Err(refuse(
                syntax.span,
                format!("the visibility {other:?} of `{name}`"),
            ))
        }
    };
    let Some(signature) = index.signature(declaration) else {
        return Err(refuse(syntax.span, format!("the signature of `{name}`")));
    };
    let body = observed.body;
    if body.parameters().len() != syntax.params.len() {
        return Err(refuse(syntax.span, format!("the parameters of `{name}`")));
    }
    let mut encoder = BodyEncoder {
        observed,
        offsets,
        calls,
        positions,
        values: HashMap::default(),
    };
    let mut parameters = Vec::new();
    for (parameter, written) in body.parameters().iter().zip(&syntax.params) {
        if let Some(what) = parameter_shape(written) {
            return Err(refuse(
                written.span,
                format!("{what} of the parameter `{}` of `{name}`", written.name),
            ));
        }
        encoder
            .values
            .insert(parameter.value, encoder.values.len() as u32);
        parameters.push(ParameterIr {
            span: plugin_span(offsets, written.span),
            name: written.name.clone(),
            ty: encoder.ty(parameter.ty.get(), written.span)?,
        });
    }
    let return_type = encoder.ty(signature.result.get(), syntax.span)?;
    let (body_span, statements) = match syntax.body {
        FunBody::Block(block) | FunBody::Expr(block) => {
            let Some(span) = observed.file.expr_span(block) else {
                return Err(refuse(syntax.span, format!("the body of `{name}`")));
            };
            (span, encoder.roots(span)?)
        }
        FunBody::None => return Err(refuse(syntax.span, format!("`{name}` without a body"))),
    };
    Ok(FunctionIr {
        span: plugin_span(offsets, syntax.span),
        name,
        visibility,
        parameters,
        return_type,
        body: BodyIr {
            span: plugin_span(offsets, body_span),
            statements,
        },
    })
}

/// The first modifier of `function` that [`FunctionIr`] does not carry, which kotlinc's IR records.
/// The others it could write are refused before this: `operator` and `infix` need a receiver,
/// `external` has no body, `inline` is not checked as an ordinary body, and `actual` needs a
/// multiplatform module.
fn modifier(function: &FunDecl) -> Option<&'static str> {
    if function.is_suspend() {
        Some("suspend")
    } else if function.is_tailrec() {
        Some("tailrec")
    } else {
        None
    }
}

/// The first type-use annotation written in `span`: on a parameter, the return type, a local, or a
/// type argument nested in any of them. [`TypeIdentity`] has room for a type's annotations, but the
/// encoder does not resolve them and the host does not build them, so an annotated type is refused
/// rather than published as the same type without them.
///
/// [`TypeIdentity`]: krusty_plugin_ir::TypeIdentity
fn type_annotation(file: &File, span: Span) -> Option<&AnnotationRef> {
    file.type_annotations
        .iter()
        .filter(|(start, _)| (span.lo..span.hi).contains(*start))
        .min_by_key(|(start, _)| **start)
        .and_then(|(_, annotations)| annotations.first())
}

/// What `parameter` has that [`ParameterIr`] does not carry, which kotlinc's IR records. Its
/// `crossinline` and `noinline` modifiers belong to an `inline` function, which is refused before
/// this.
fn parameter_shape(parameter: &Param) -> Option<&'static str> {
    if !parameter.annotations.is_empty() {
        Some("the annotations")
    } else if parameter.is_vararg {
        Some("the `vararg` modifier")
    } else if parameter.default.is_some() {
        Some("the default value")
    } else {
        None
    }
}

struct BodyEncoder<'a, 'o> {
    observed: &'a ObservedBody<'o>,
    offsets: &'a Utf16Offsets,
    calls: &'a CallSpans,
    positions: &'a HashMap<DeclarationId, (u32, u32)>,
    /// The plugin IR's number for each value the body reads or writes: its parameters, then its
    /// locals in declaration order.
    values: HashMap<LocalValueId, u32>,
}

impl BodyEncoder<'_, '_> {
    fn refuse(&self, span: Span, what: impl Into<String>) -> Refusal {
        Refusal {
            source: self.observed.source,
            span,
            what: what.into(),
        }
    }

    fn ty(&self, ty: Ty, span: Span) -> Result<krusty_plugin_ir::TypeIdentity, Refusal> {
        identity(ty, TypeParameters::NONE).map_err(|what| self.refuse(span, what))
    }

    /// The source span `origin` names, refusing a compiler-generated one: the plugin IR gives
    /// every element the offsets of the source that wrote it.
    fn span(&self, origin: crate::fir::OriginId) -> Result<Span, Refusal> {
        match self.observed.source_map.origins().get(origin) {
            Some(Origin::Source { span, .. }) => Ok(span),
            Some(Origin::Synthetic { cause, kind }) => {
                let span = self
                    .observed
                    .source_map
                    .origins()
                    .source_span(cause)
                    .map_or(Span::new(0, 0), |(_, span)| span);
                Err(self.refuse(span, format!("compiler-generated code ({kind:?})")))
            }
            None => Err(self.refuse(Span::new(0, 0), "code with no recorded origin")),
        }
    }

    /// The body's statements: a block body's own, or an expression body's expression returned at
    /// the body's end, as kotlinc builds one.
    fn roots(&mut self, body_span: Span) -> Result<Vec<StatementIr>, Refusal> {
        let body = self.observed.body;
        if body.has_implicit_return() {
            let [root] = body.roots() else {
                return Err(self.refuse(body_span, "an expression body of several roots"));
            };
            let Some(FirStatementKind::Expression(value)) =
                body.statement(*root).map(|statement| &statement.kind)
            else {
                return Err(self.refuse(body_span, "an expression body that is not an expression"));
            };
            let value = self.expression(*value)?;
            let end = plugin_span(self.offsets, Span::new(body_span.hi, body_span.hi));
            return Ok(vec![StatementIr::Expression(ExpressionIr {
                span: end,
                kind: ExpressionKind::Return(Box::new(value)),
            })]);
        }
        let mut statements = Vec::new();
        for &root in body.roots() {
            match body.statement(root).map(|statement| &statement.kind) {
                Some(FirStatementKind::Expression(expression)) => {
                    match body.expr(*expression).map(|expression| &expression.kind) {
                        Some(FirExprKind::Block {
                            statements: block,
                            result,
                        }) => {
                            for &statement in block.iter() {
                                statements.push(self.statement(statement)?);
                            }
                            if let Some(result) = result {
                                statements.push(StatementIr::Expression(self.expression(*result)?));
                            }
                        }
                        _ => statements.push(self.statement(root)?),
                    }
                }
                _ => statements.push(self.statement(root)?),
            }
        }
        Ok(statements)
    }

    fn statement(&mut self, id: FirStatementId) -> Result<StatementIr, Refusal> {
        let body = self.observed.body;
        let Some(statement) = body.statement(id) else {
            return Err(self.refuse(Span::new(0, 0), "a statement missing from its body"));
        };
        let span = self.span(statement.origin)?;
        match &statement.kind {
            FirStatementKind::Expression(expression) => {
                Ok(StatementIr::Expression(self.expression(*expression)?))
            }
            FirStatementKind::Local {
                target,
                ty,
                mutable,
                lateinit,
                deferred,
                initializer,
                conversion,
            } => {
                if *lateinit {
                    return Err(self.refuse(span, "a `lateinit` local"));
                }
                let Some(name) = body.debug_value_name(*target) else {
                    return Err(self.refuse(span, "a local the compiler introduced"));
                };
                let initializer = match (deferred, initializer) {
                    (false, Some(initializer)) => {
                        Some(self.converted(*initializer, *conversion, span)?)
                    }
                    _ => None,
                };
                let next = self.values.len() as u32;
                self.values.insert(*target, next);
                Ok(StatementIr::Variable(VariableIr {
                    span: plugin_span(self.offsets, span),
                    name: name.to_string(),
                    ty: self.ty(ty.get(), span)?,
                    mutable: *mutable,
                    initializer,
                }))
            }
            other => Err(self.refuse(span, format!("the statement {}", statement_kind(other)))),
        }
    }

    /// `value` as its consumer receives it. kotlinc's IR spells no widening to a nullable
    /// supertype, since every value of a type is a value of its supertypes; any other conversion
    /// is an IR element of its own.
    fn converted(
        &mut self,
        value: FirExprId,
        conversion: Option<FirConversion>,
        span: Span,
    ) -> Result<ExpressionIr, Refusal> {
        match conversion.map(|conversion| conversion.kind) {
            None | Some(FirConversionKind::NullabilityWidening { .. }) => self.expression(value),
            Some(other) => {
                Err(self.refuse(span, format!("the conversion {}", conversion_kind(&other))))
            }
        }
    }

    fn value(&self, value: LocalValueId, span: Span) -> Result<ValueId, Refusal> {
        self.values
            .get(&value)
            .map(|&id| ValueId(id))
            .ok_or_else(|| self.refuse(span, "a value the compiler introduced"))
    }

    fn expression(&mut self, id: FirExprId) -> Result<ExpressionIr, Refusal> {
        let body = self.observed.body;
        let Some(expression) = body.expr(id) else {
            return Err(self.refuse(Span::new(0, 0), "an expression missing from its body"));
        };
        let span = self.span(expression.origin)?;
        let kind = match &expression.kind {
            FirExprKind::Constant(constant) => {
                if body.is_constant_read(id) {
                    return Err(self.refuse(span, "a read of a constant"));
                }
                ExpressionKind::Constant(self.constant(constant, expression.ty.get(), span)?)
            }
            FirExprKind::ValueRead(value) => ExpressionKind::GetValue(self.value(*value, span)?),
            FirExprKind::ValueWrite {
                target,
                value,
                conversion,
            } => {
                let target = self.value(*target, span)?;
                ExpressionKind::SetValue {
                    value: target,
                    to: Box::new(self.converted(*value, *conversion, span)?),
                }
            }
            FirExprKind::Jump {
                kind: FirJumpKind::Return { target_depth: 0 },
                value: Some(value),
                ..
            } => ExpressionKind::Return(Box::new(self.expression(*value)?)),
            FirExprKind::Call(call) => {
                let call = self.call(call, expression.ty.get(), span)?;
                let Some(written) = self.calls.get(span) else {
                    return Err(self.refuse(span, "a call not written as a call"));
                };
                return Ok(ExpressionIr {
                    span: plugin_span(self.offsets, written),
                    kind: ExpressionKind::Call(Box::new(call)),
                });
            }
            other => {
                return Err(self.refuse(span, format!("the expression {}", expression_kind(other))))
            }
        };
        Ok(ExpressionIr {
            span: plugin_span(self.offsets, span),
            kind,
        })
    }

    fn constant(&self, constant: &FirConstant, ty: Ty, span: Span) -> Result<Constant, Refusal> {
        Ok(match constant {
            FirConstant::Boolean(value) => Constant::Boolean(*value),
            FirConstant::Int(value) if ty == Ty::Int => Constant::Int(
                i32::try_from(*value)
                    .map_err(|_| self.refuse(span, "an Int constant out of range"))?,
            ),
            FirConstant::Long(value) => Constant::Long(*value),
            FirConstant::String(value) => Constant::String(
                value
                    .as_str()
                    .ok_or_else(|| self.refuse(span, "a string with an unpaired surrogate"))?
                    .to_string(),
            ),
            FirConstant::Null => Constant::Null(self.ty(ty, span)?),
            other => {
                return Err(self.refuse(span, format!("the constant {other:?} of type {ty:?}")))
            }
        })
    }

    fn call(&mut self, call: &FirCall, ty: Ty, span: Span) -> Result<CallIr, Refusal> {
        let (callee, layout) = match &call.target {
            FirCallTarget::Module(callable) => self.module_callee(*callable, span)?,
            FirCallTarget::External { declaration, .. } => {
                let receiver = match &call.dispatch_receiver {
                    Some(receiver) => Some(self.receiver_class(receiver.value, span)?),
                    None => None,
                };
                self.external_callee(*declaration, receiver, span)?
            }
            FirCallTarget::Intrinsic { .. } => {
                return Err(self.refuse(span, "a call to a compiler intrinsic"))
            }
            FirCallTarget::Classifier { .. } => {
                return Err(self.refuse(span, "a call through a classifier"))
            }
            FirCallTarget::Super { .. } => return Err(self.refuse(span, "a `super` call")),
        };
        let mut arguments: Vec<Option<ExpressionIr>> = Vec::new();
        match (layout.dispatch, &call.dispatch_receiver) {
            (true, Some(receiver)) => arguments.push(Some(self.converted(
                receiver.value,
                receiver.conversion,
                span,
            )?)),
            (false, None) => {}
            _ => return Err(self.refuse(span, "a call's dispatch receiver")),
        }
        match (layout.extension, &call.extension_receiver) {
            (true, Some(receiver)) => arguments.push(Some(self.converted(
                receiver.value,
                receiver.conversion,
                span,
            )?)),
            (false, None) => {}
            _ => return Err(self.refuse(span, "a call's extension receiver")),
        }
        let first = arguments.len();
        arguments.resize_with(first + layout.values, || None);
        for argument in call.arguments.iter() {
            match argument {
                FirCallArgument::Expression {
                    parameter,
                    value,
                    conversion,
                } if (*parameter as usize) < layout.values => {
                    arguments[first + *parameter as usize] =
                        Some(self.converted(*value, *conversion, span)?);
                }
                FirCallArgument::Default { parameter, .. }
                    if (*parameter as usize) < layout.values => {}
                FirCallArgument::Expression { .. } | FirCallArgument::Default { .. } => {
                    return Err(self.refuse(span, "an argument beyond the callee's parameters"))
                }
                FirCallArgument::Vararg { .. } => {
                    return Err(self.refuse(span, "a vararg argument"))
                }
            }
        }
        let mut type_arguments = vec![None; layout.type_parameters];
        for substitution in call.substitutions.iter() {
            let ordinal = match substitution.parameter {
                FirTypeParameterRef::External { ordinal, .. } => ordinal as usize,
                FirTypeParameterRef::Module(_) => {
                    return Err(self.refuse(span, "a type argument of a module function"))
                }
            };
            let Some(slot) = type_arguments.get_mut(ordinal) else {
                return Err(self.refuse(span, "a type argument beyond the callee's"));
            };
            *slot = Some(self.ty(substitution.value.get(), span)?);
        }
        let type_arguments = type_arguments
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| self.refuse(span, "a call with a type argument left uninferred"))?;
        Ok(CallIr {
            callee,
            ty: self.ty(ty, span)?,
            type_arguments,
            arguments,
        })
    }

    fn module_callee(
        &self,
        callable: crate::fir::CallableId,
        span: Span,
    ) -> Result<(Callee, Layout), Refusal> {
        let index = self.observed.index;
        let header = index
            .callable(callable)
            .ok_or_else(|| self.refuse(span, "a call to an unknown module function"))?;
        let &(file, function) = self.positions.get(&header.declaration).ok_or_else(|| {
            self.refuse(
                span,
                "a call to a module declaration that is not a top-level function",
            )
        })?;
        let values = index
            .signature(header.declaration)
            .map(|signature| signature.parameters.len())
            .ok_or_else(|| self.refuse(span, "a call to a module function without a signature"))?;
        Ok((
            Callee::Module { file, function },
            Layout {
                dispatch: false,
                extension: false,
                values,
                type_parameters: 0,
            },
        ))
    }

    /// The class a member call's dispatch receiver has: kotlinc's IR calls the member as that
    /// class sees it, a fake override where the class inherits it.
    fn receiver_class(&self, receiver: FirExprId, span: Span) -> Result<TypeName, Refusal> {
        let Some(expression) = self.observed.body.expr(receiver) else {
            return Err(self.refuse(span, "a receiver missing from its body"));
        };
        {
            let other = expression.ty.get();
            match other.kind() {
                TyKind::Obj(class, _) => Ok(class),
                _ => Err(self.refuse(
                    span,
                    format!("a member call on a receiver of type {other:?}"),
                )),
            }
        }
    }

    /// The dependency callable `declaration`, named for a member call by `receiver`'s class.
    fn external_callee(
        &self,
        declaration: crate::fir::ExternalCallableId,
        receiver: Option<TypeName>,
        span: Span,
    ) -> Result<(Callee, Layout), Refusal> {
        let Some(realization) = self
            .observed
            .symbols
            .semantic_platform()
            .external_callable(declaration)
        else {
            return Err(self.refuse(
                span,
                "a call to a dependency the platform does not describe",
            ));
        };
        let callable = &realization.callable;
        let (dispatch, extension) = match realization.kind {
            ExternalCallableKind::TopLevel => (false, false),
            ExternalCallableKind::Extension => (false, true),
            ExternalCallableKind::Member => (true, false),
            other => return Err(self.refuse(span, format!("a call realized as {other:?}"))),
        };
        if callable.external_property_identity.is_some() {
            return Err(self.refuse(span, "a call to a property accessor"));
        }
        if callable.context_count != 0 {
            return Err(self.refuse(span, "a call to a function with context parameters"));
        }
        let (package, classes) = match realization.declaration_owner {
            Some(SemanticCallableOwner::Package(package)) if !dispatch => {
                (package_name(package), Vec::new())
            }
            Some(SemanticCallableOwner::Classifier(_)) if dispatch => {
                let Some(receiver) = receiver else {
                    return Err(self.refuse(span, "a member call without its receiver"));
                };
                let class = class_name(receiver);
                (class.package, class.classes)
            }
            _ => return Err(self.refuse(span, "a call to a dependency of unknown owner")),
        };
        let formals: &[crate::types::TyParamName] = callable
            .generic_sig
            .as_ref()
            .map_or(&[], |signature| &signature.formals);
        let declared = callable.declared_params.as_deref().ok_or_else(|| {
            self.refuse(span, "a call to a dependency without declared parameters")
        })?;
        let parameters = TypeParameters(formals);
        let declared = declared
            .iter()
            .map(|ty| identity(*ty, parameters).map_err(|what| self.refuse(span, what)))
            .collect::<Result<Vec<_>, _>>()?;
        let (extension_receiver, value_parameters) = if extension {
            let Some((receiver, values)) = declared.split_first() else {
                return Err(self.refuse(span, "an extension without its receiver"));
            };
            (Some(receiver.clone()), values.to_vec())
        } else {
            (None, declared)
        };
        let values = value_parameters.len();
        Ok((
            Callee::External(Box::new(CallableIdentity {
                kind: CallableKind::Function {
                    name: callable.name.clone(),
                },
                package,
                classes,
                type_parameters: formals.len() as u32,
                context_parameters: Vec::new(),
                extension_receiver,
                value_parameters,
            })),
            Layout {
                dispatch,
                extension,
                values,
                type_parameters: formals.len(),
            },
        ))
    }
}

/// Where a callee's arguments go in kotlinc's parameter order.
struct Layout {
    dispatch: bool,
    extension: bool,
    values: usize,
    type_parameters: usize,
}

fn conversion_kind(kind: &FirConversionKind) -> &'static str {
    match kind {
        FirConversionKind::NumericWidening { .. } => "numeric widening",
        FirConversionKind::NumericConversion { .. } => "numeric conversion",
        FirConversionKind::NullabilityWidening { .. } => "nullability widening",
        FirConversionKind::SmartCast { .. } => "smart cast",
        FirConversionKind::Sam(_) => "SAM conversion",
        FirConversionKind::PlatformNarrowing { .. } => "platform-type narrowing",
        FirConversionKind::FunctionValue { .. } => "function value conversion",
        FirConversionKind::CoerceToUnit => "coercion to Unit",
    }
}

fn statement_kind(kind: &FirStatementKind) -> &'static str {
    match kind {
        FirStatementKind::Local { .. } => "local",
        FirStatementKind::Destructure { .. } => "destructuring declaration",
        FirStatementKind::ConstructorDelegation(_) => "constructor delegation",
        FirStatementKind::InterfaceDelegationInitializer { .. } => "interface delegation",
        FirStatementKind::Expression(_) => "expression",
        FirStatementKind::Loop { .. } => "loop",
        FirStatementKind::LocalTypeAlias => "local type alias",
        FirStatementKind::LocalDeclaration { .. } => "local class",
        FirStatementKind::LocalFunction { .. } => "local function",
    }
}

fn expression_kind(kind: &FirExprKind) -> String {
    // The variant's name says which construct it is; its fields are checked FIR, not source.
    let debug = format!("{kind:?}");
    let name = debug
        .split(|character: char| !character.is_alphanumeric())
        .next()
        .unwrap_or_default();
    format!("`{name}`")
}

/// `span` of the file as kotlinc's offsets into the text it reads.
fn plugin_span(offsets: &Utf16Offsets, span: Span) -> krusty_plugin_ir::Span {
    krusty_plugin_ir::Span {
        start: offsets.unit(span.lo),
        end: offsets.unit(span.hi),
    }
}
