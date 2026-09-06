//! Stable body-unit routing over one transiently parsed source file.

use super::constructors::{
    check_and_dispatch_constructor_body, check_and_dispatch_signature_constructor_defaults,
};
use super::*;
use crate::ast::{
    setter_param_or_value, AstEnumEntry, ClassDecl, Decl, FunBody, FunDecl, PropDecl,
};
use crate::resolve::ResolvedConstructor;

/// The receiver on a companion-block declaration is an associated-classifier lookup coordinate,
/// not a value available to the declaration body. Keep it out of checked FIR receiver slots just as
/// call FIR keeps it out of dispatch/extension operands.
pub(super) fn body_extension_receiver(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    receiver: Option<ResolvedTy>,
) -> Option<ResolvedTy> {
    if index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION))
    {
        None
    } else {
        receiver
    }
}

fn function_at_anchor<'a>(file: &'a File, range: Span) -> Option<&'a FunDecl> {
    file.decl_arena
        .iter()
        .find_map(|declaration| match declaration {
            Decl::Fun(function) if function.span == range => Some(function),
            Decl::Class(class) => class
                .methods
                .iter()
                .chain(
                    class
                        .enum_entries
                        .iter()
                        .flat_map(|entry| entry.methods.iter()),
                )
                .find(|function| function.span == range),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
}

fn property_at_anchor(file: &File, range: Span) -> Option<&PropDecl> {
    file.decl_arena
        .iter()
        .find_map(|declaration| match declaration {
            Decl::Property(property) if property.span == range => Some(property),
            Decl::Class(class) => class
                .body_props
                .iter()
                .chain(
                    class
                        .enum_entries
                        .iter()
                        .flat_map(|entry| entry.props.iter()),
                )
                .find(|property| property.span == range),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
}

pub(super) fn class_at_anchor(file: &File, range: Span) -> Option<&ClassDecl> {
    file.decl_arena
        .iter()
        .find_map(|declaration| match declaration {
            Decl::Class(class) if class.span == range => Some(class),
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
}

fn property_for_work<'a>(
    file: &'a File,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    active: Option<&ActiveSourceDeclarations>,
) -> Option<(DeclarationId, &'a PropDecl)> {
    let declaration = match work.kind {
        BodyKind::Initializer | BodyKind::Delegate => work.declaration,
        BodyKind::Getter | BodyKind::Setter => index.declaration_anchor(work.declaration)?.owner?,
        BodyKind::Function | BodyKind::EnumEntry | BodyKind::Constructor | BodyKind::Script => {
            return None
        }
    };
    let property = match active {
        Some(active) => active.property(file, declaration)?,
        None => {
            let range = index.declaration_range(declaration)?;
            property_at_anchor(file, range)?
        }
    };
    Some((declaration, property))
}

fn property_body_root(property: &PropDecl, kind: BodyKind) -> Option<crate::ast::ExprId> {
    match kind {
        BodyKind::Initializer => property.init,
        BodyKind::Delegate => property.delegate,
        BodyKind::Getter => property.getter.as_ref().and_then(fun_body_root),
        BodyKind::Setter => property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
            .and_then(fun_body_root),
        BodyKind::Function | BodyKind::EnumEntry | BodyKind::Constructor | BodyKind::Script => None,
    }
}

fn fun_body_root(body: &FunBody) -> Option<crate::ast::ExprId> {
    match body {
        FunBody::Expr(root) | FunBody::Block(root) => Some(*root),
        FunBody::None => None,
    }
}

fn expression_at_range(file: &File, range: Span) -> Option<crate::ast::ExprId> {
    file.expr_spans
        .iter()
        .position(|span| *span == range)
        .map(|index| crate::ast::ExprId(index as u32))
}

fn enclosing_classifier_declaration(
    index: &ResolvedModuleIndex,
    mut declaration: DeclarationId,
) -> Option<DeclarationId> {
    loop {
        let anchor = index.declaration_anchor(declaration)?;
        if anchor.kind == DeclarationKind::Classifier {
            return Some(declaration);
        }
        declaration = anchor.owner?;
    }
}

/// Primary-constructor values visible while class initialization runs. These are body-local
/// bindings in the resolver's checked result, so every independently streamed initializer unit must
/// bind the same stable source parameters before translating its expressions.
fn class_initialization_parameters<'a>(
    file: &'a File,
    info: &TypeInfo,
    class_declaration: DeclarationId,
    index: &ResolvedModuleIndex,
    active: Option<&ActiveSourceDeclarations>,
) -> Result<Vec<CheckedBodyParameter<'a>>, CheckedBodyDriverFailure> {
    let class = match active {
        Some(active) => active
            .class(file, class_declaration)
            .map(|(_, class)| class),
        None => index
            .declaration_anchor(class_declaration)
            .filter(|anchor| anchor.kind == DeclarationKind::Classifier)
            .and_then(|_| index.declaration_range(class_declaration))
            .and_then(|range| class_at_anchor(file, range)),
    }
    .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    class
        .props
        .iter()
        .map(|parameter| {
            let ty = info
                .resolved_type(&parameter.ty)
                .ok_or(CheckedBodyDriverFailure::ParameterShapeMismatch)?;
            let ty = crate::fir::ResolvedTy::new(ty).map_err(|error| {
                CheckedBodyDriverFailure::Check(BodyCheckFailure {
                    span: Some(parameter.ty.span),
                    kind: BodyCheckFailureKind::UnpublishableType(error),
                })
            })?;
            Ok(CheckedBodyParameter {
                name: parameter.name.as_str(),
                ty,
                span: parameter.span,
            })
        })
        .collect()
}

/// Check and route any body-unit shape currently representable by checked FIR. The match is
/// exhaustive so adding a parser/header body kind cannot silently bypass FIR construction.
#[allow(clippy::too_many_arguments)]
pub fn check_and_dispatch_body(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
) -> Result<(), CheckedBodyDriverFailure> {
    let mut cursor = crate::fir::ActiveSourceCursor::new(source, index);
    let active = cursor
        .bind_next(file, source, index)
        .expect("focused FIR body checking must bind the live parser arena");
    assert!(
        cursor.is_finished(),
        "focused FIR body checking must bind the complete live parser arena"
    );
    let mut session = BodyCheckSession::default();
    check_and_dispatch_active_body_in_session(
        file,
        &active,
        info,
        source,
        work,
        index,
        origins,
        inline_bodies,
        ordinary_sink,
        &mut session,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn check_and_dispatch_bound_body_in_session(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    let mut cursor = crate::fir::ActiveSourceCursor::new(source, index);
    let active = cursor
        .bind_next(file, source, index)
        .expect("focused FIR body checking must bind the live parser arena");
    assert!(
        cursor.is_finished(),
        "focused FIR body checking must bind the complete live parser arena"
    );
    check_and_dispatch_active_body_in_session(
        file,
        &active,
        info,
        source,
        work,
        index,
        origins,
        inline_bodies,
        ordinary_sink,
        session,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn check_and_dispatch_body_in_session(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    check_and_dispatch_body_in_session_with_source(
        file,
        None,
        info,
        source,
        work,
        index,
        origins,
        inline_bodies,
        ordinary_sink,
        session,
    )
}

/// Pass-2 entry point. Syntax is obtained only from the fresh parser binding stream, never from a
/// stable source range retained by Pass 1.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_and_dispatch_active_body_in_session(
    file: &File,
    active: &ActiveSourceDeclarations,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    session.install_active_source(active);
    check_and_dispatch_body_in_session_with_source(
        file,
        Some(active),
        info,
        source,
        work,
        index,
        origins,
        inline_bodies,
        ordinary_sink,
        session,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_and_dispatch_body_in_session_with_source(
    file: &File,
    active: Option<&ActiveSourceDeclarations>,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    crate::trace_compiler!(
        "fir",
        "check body declaration={:?} kind={:?} name={:?} anchor={:?}",
        work.declaration,
        work.kind,
        index.declaration_name(work.declaration),
        index.declaration_anchor(work.declaration),
    );
    let result = match work.kind {
        BodyKind::Function => check_and_dispatch_scheduled_function_body_in_session_with_source(
            file,
            active,
            info,
            source,
            work,
            index,
            origins,
            inline_bodies,
            ordinary_sink,
            session,
        ),
        BodyKind::Initializer => match index.declaration_anchor(work.declaration) {
            Some(anchor) if anchor.kind == DeclarationKind::Initializer => {
                let root = match active {
                    Some(active) => active.expression(file, work.declaration),
                    None => index
                        .declaration_range(work.declaration)
                        .and_then(|range| expression_at_range(file, range)),
                }
                .ok_or(CheckedBodyDriverFailure::MissingBody)?;
                let class = anchor
                    .owner
                    .and_then(|owner| enclosing_classifier_declaration(index, owner))
                    .ok_or(CheckedBodyDriverFailure::MissingBody)?;
                let parameters = class_initialization_parameters(file, info, class, index, active)?;
                let body = check_expression_body_with_parameters_in_session(
                    file,
                    info,
                    source,
                    work.owner,
                    root,
                    &parameters,
                    index,
                    origins,
                    session,
                )
                .map_err(CheckedBodyDriverFailure::Check)?;
                ordinary_sink.accept(work.owner, body);
                Ok(())
            }
            Some(_) => check_and_dispatch_property_body(
                file,
                info,
                source,
                work,
                index,
                origins,
                inline_bodies,
                ordinary_sink,
                session,
                active,
            ),
            None => Err(CheckedBodyDriverFailure::MissingBody),
        },
        BodyKind::Delegate | BodyKind::Getter | BodyKind::Setter => {
            check_and_dispatch_property_body(
                file,
                info,
                source,
                work,
                index,
                origins,
                inline_bodies,
                ordinary_sink,
                session,
                active,
            )
        }
        BodyKind::Script => {
            let root = file
                .script_body
                .ok_or(CheckedBodyDriverFailure::MissingBody)?;
            let body = check_expression_body_with_parameters_in_session(
                file,
                info,
                source,
                work.owner,
                root,
                &[],
                index,
                origins,
                session,
            )
            .map_err(CheckedBodyDriverFailure::Check)?;
            ordinary_sink.accept(work.owner, body);
            Ok(())
        }
        BodyKind::EnumEntry => check_and_dispatch_enum_entry(
            file,
            info,
            source,
            work,
            index,
            origins,
            ordinary_sink,
            session,
            active,
        ),
        BodyKind::Constructor => check_and_dispatch_constructor_body(
            file,
            info,
            source,
            work,
            index,
            origins,
            ordinary_sink,
            session,
            active,
        ),
    };
    if let Err(error) = &result {
        crate::trace_compiler!(
            "fir",
            "body failed declaration={:?} kind={:?}: {error:?}",
            work.declaration,
            work.kind,
        );
    }
    result
}

fn enum_entry_for_work<'a>(
    file: &'a File,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    active: Option<&ActiveSourceDeclarations>,
) -> Option<(DeclarationId, &'a ClassDecl, &'a AstEnumEntry)> {
    let entry_anchor = index.declaration_anchor(work.declaration)?;
    let class_declaration = entry_anchor.owner?;
    let (class, entry) = match active {
        Some(active) => (
            active.class(file, class_declaration)?.1,
            active.enum_entry(file, work.declaration)?,
        ),
        None => {
            index.declaration_anchor(class_declaration)?;
            let class = class_at_anchor(file, index.declaration_range(class_declaration)?)?;
            let entry = class
                .enum_entries
                .iter()
                .find(|entry| index.declaration_range(work.declaration) == Some(entry.span))?;
            (class, entry)
        }
    };
    Some((class_declaration, class, entry))
}

#[allow(clippy::too_many_arguments)]
fn check_and_dispatch_enum_entry(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
    active: Option<&ActiveSourceDeclarations>,
) -> Result<(), CheckedBodyDriverFailure> {
    let anchor = index
        .declaration_anchor(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    if anchor.source != source {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let (_class_declaration, _class, entry) = enum_entry_for_work(file, work, index, active)
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    let selected = info
        .resolved_enum_entry_constructors
        .get(&(entry.span.lo, entry.span.hi))
        .cloned()
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let ResolvedConstructor::Source {
        owner,
        stable_declaration,
        outer,
        primary,
        params,
        argument_slots,
        omitted,
        vararg,
        ..
    } = selected
    else {
        return Err(CheckedBodyDriverFailure::MissingCallable);
    };
    if outer.is_some() || argument_slots.len() != entry.args.len() {
        return Err(CheckedBodyDriverFailure::ParameterShapeMismatch);
    }
    let declaration = stable_declaration
        .filter(|declaration| index.callable_for_declaration(*declaration).is_some())
        .or_else(|| index.constructor_declaration(owner, primary, &params))
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let constructor = index
        .callable_for_declaration(declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let signature = index
        .signature(constructor.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;

    let mut checker = BodyFirChecker::new(
        file, info, source, work.owner, entry.span, index, origins, session,
    );
    let cause = checker.origins.source(source, entry.span);
    let arguments = checker
        .checked_constructor_arguments_at(
            entry.span,
            &entry.args,
            &params,
            argument_slots,
            omitted,
            vararg,
        )
        .map_err(CheckedBodyDriverFailure::Check)?;
    let expression = checker.body.add_expr(FirExpr {
        origin: cause,
        ty: signature.result,
        kind: FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::Module(constructor.id),
            context_parameter_count: 0,
            outer_parameter: None,
            outer_receiver: None,
            external_capture_arguments: None,
            parameter_types: checker
                .published_parameter_types(Some(entry.span), &params)
                .map_err(CheckedBodyDriverFailure::Check)?,
            arguments,
            substitutions: Box::new([]),
        }),
    });
    let statement = checker.body.add_statement(FirStatement {
        origin: cause,
        kind: FirStatementKind::Expression(expression),
    });
    checker.body.push_root(statement);
    ordinary_sink.accept(work.owner, checker.body);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_and_dispatch_property_body(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
    active: Option<&ActiveSourceDeclarations>,
) -> Result<(), CheckedBodyDriverFailure> {
    let anchor = index
        .declaration_anchor(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    if anchor.source != source {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let (property_declaration, property) = property_for_work(file, work, index, active)
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    let root =
        property_body_root(property, work.kind).ok_or(CheckedBodyDriverFailure::MissingBody)?;
    let setter_name = (work.kind == BodyKind::Setter).then(|| {
        setter_param_or_value(
            property
                .setter
                .as_ref()
                .and_then(|setter| setter.param.as_ref()),
        )
    });
    let property_id = index
        .property_for_declaration(property_declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let property_header = index
        .property(property_id)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let signature = index
        .signature(property_declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let context_receivers = signature
        .parameters
        .get(..property_header.context_parameter_count as usize)
        .ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "property body indexed parameter mismatch declaration={property_declaration:?} kind={:?} indexed_context={} signature_parameters={}",
                work.kind,
                property_header.context_parameter_count,
                signature.parameters.len(),
            );
            CheckedBodyDriverFailure::ParameterShapeMismatch
        })?;
    if property.context_params.len() != context_receivers.len() {
        crate::trace_compiler!(
            "fir",
            "property body parameter mismatch declaration={property_declaration:?} kind={:?} ast_context={} indexed_context={} signature_parameters={}",
            work.kind,
            property.context_params.len(),
            property_header.context_parameter_count,
            signature.parameters.len(),
        );
        return Err(CheckedBodyDriverFailure::ParameterShapeMismatch);
    }
    let mut parameters = property
        .context_params
        .iter()
        .zip(context_receivers.iter().copied())
        .map(|(parameter, ty)| CheckedBodyParameter {
            name: parameter.name.as_str(),
            ty,
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    // A member property initializer or delegate expression runs inside the constructor, where the primary-constructor
    // parameters are ordinary locals — that is exactly how the checker types them
    // (`class A(val y: Int) { var x = y }` reads `y` with `origin = Local`). The body unit therefore
    // has to carry them, or the checked body has no binding for a name the checker resolved.
    if matches!(work.kind, BodyKind::Initializer | BodyKind::Delegate) {
        if let Some(class) = index
            .declaration_anchor(property_declaration)
            .and_then(|anchor| anchor.owner)
            .filter(|owner| {
                index
                    .declaration_anchor(*owner)
                    .is_some_and(|anchor| anchor.kind == DeclarationKind::Classifier)
            })
        {
            parameters.extend(class_initialization_parameters(
                file, info, class, index, active,
            )?);
        }
    }
    if let Some(name) = setter_name.as_deref() {
        parameters.push(CheckedBodyParameter {
            name,
            ty: signature.result,
            span: property.span,
        });
    }
    let property_storage_type = property
        .explicit_backing_field
        .as_ref()
        .and_then(|_| {
            info.explicit_backing_field_types
                .get(&(property.span.lo, property.span.hi))
                .copied()
        })
        .map(|storage| {
            crate::fir::ResolvedTy::new(storage)
                .expect("checked explicit backing-field storage must be publishable")
        });
    let mut body = check_body_unit_with_parameters_and_defaults(
        file,
        info,
        source,
        work.owner,
        file.expr_span(root)
            .ok_or(CheckedBodyDriverFailure::MissingBody)?,
        Some(root),
        &parameters,
        &[],
        CheckedBodyReceiverShape {
            context_receivers,
            context_value_count: property_header.context_value_count,
            extension_receiver: body_extension_receiver(
                index,
                property_declaration,
                property_header.extension_receiver,
            ),
        },
        (work.kind == BodyKind::Initializer)
            .then_some(property_storage_type)
            .flatten(),
        match work.kind {
            BodyKind::Initializer => Some(property_storage_type.unwrap_or(signature.result)),
            BodyKind::Getter if matches!(property.getter, Some(FunBody::Expr(_))) => {
                Some(signature.result)
            }
            BodyKind::Setter
                if property
                    .setter
                    .as_ref()
                    .and_then(|setter| setter.body.as_ref())
                    .is_some_and(|body| matches!(body, FunBody::Expr(_))) =>
            {
                Some(
                    crate::fir::ResolvedTy::new(crate::types::Ty::Unit)
                        .expect("Unit is a publishable FIR type"),
                )
            }
            BodyKind::Delegate
            | BodyKind::Getter
            | BodyKind::Setter
            | BodyKind::Function
            | BodyKind::EnumEntry
            | BodyKind::Constructor
            | BodyKind::Script => None,
        },
        index,
        origins,
        session,
    )
    .map_err(CheckedBodyDriverFailure::Check)?;
    match work.kind {
        BodyKind::Initializer => {
            let storage = property_storage_type;
            publish_result_type(&mut body, storage.unwrap_or(signature.result))?;
            if let Some(storage) = storage {
                body.set_property_storage_type(storage);
            }
        }
        BodyKind::Getter => publish_result_type(&mut body, signature.result)?,
        BodyKind::Setter => publish_result_type(
            &mut body,
            crate::fir::ResolvedTy::new(crate::types::Ty::Unit)
                .expect("Unit is a publishable FIR type"),
        )?,
        BodyKind::Delegate => body.set_property_delegate(
            super::delegates::property_delegate_plan(file, info, index, root, property.is_var)
                .map_err(CheckedBodyDriverFailure::Check)?,
        ),
        BodyKind::Function | BodyKind::EnumEntry | BodyKind::Constructor | BodyKind::Script => {
            return Err(CheckedBodyDriverFailure::UnsupportedBodyKind(work.kind));
        }
    }
    if work.kind == BodyKind::Getter && matches!(property.getter, Some(FunBody::Expr(_))) {
        body.set_implicit_return();
    }
    session.absorb_checked_body(&body);
    if let Some(callable) = index.callable_for_declaration(work.declaration) {
        crate::fir::dispatch_checked_body(callable, work, body, inline_bodies, ordinary_sink);
    } else {
        ordinary_sink.accept(work.owner, body);
    }
    Ok(())
}

/// Same-parse adapter for checking one function selected by its stable declaration identity.
/// Production Pass 2 uses `ActiveSourceDeclarations` instead of searching by this diagnostic span.
#[allow(clippy::too_many_arguments)]
pub fn check_and_dispatch_scheduled_function_body(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
) -> Result<(), CheckedBodyDriverFailure> {
    let mut session = BodyCheckSession::default();
    check_and_dispatch_scheduled_function_body_in_session(
        file,
        info,
        source,
        work,
        index,
        origins,
        inline_bodies,
        ordinary_sink,
        &mut session,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn check_and_dispatch_scheduled_function_body_in_session(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    check_and_dispatch_scheduled_function_body_in_session_with_source(
        file,
        None,
        info,
        source,
        work,
        index,
        origins,
        inline_bodies,
        ordinary_sink,
        session,
    )
}

#[allow(clippy::too_many_arguments)]
fn check_and_dispatch_scheduled_function_body_in_session_with_source(
    file: &File,
    active: Option<&ActiveSourceDeclarations>,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    inline_bodies: &mut InlineBodyStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    let anchor = index
        .declaration_anchor(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    if anchor.source != source {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let function = match active {
        Some(active) => active.function(file, work.declaration),
        None => index
            .declaration_range(work.declaration)
            .and_then(|range| function_at_anchor(file, range)),
    }
    .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    let root = fun_body_root(&function.body);
    if root.is_none()
        && function
            .params
            .iter()
            .all(|parameter| parameter.default.is_none())
    {
        return Err(CheckedBodyDriverFailure::MissingBody);
    }
    // A function work item identifies the complete declaration, not just its body expression:
    // default arguments live in the header and must be present when the callable body is checked.
    // Pass-1-only callers still validate their same-pass anchor. Pass 2 has already structurally
    // rebound this declaration through `ActiveSourceDeclarations` and never consults the range.
    if active.is_none() && Some(function.span) != index.declaration_range(work.declaration) {
        return Err(CheckedBodyDriverFailure::BodyRangeMismatch);
    }
    let signature = index.signature(work.declaration).ok_or_else(|| {
        crate::trace_compiler!(
            "fir",
            "function body has no finalized signature declaration={:?} name={:?}",
            work.declaration,
            index.declaration_name(work.declaration),
        );
        CheckedBodyDriverFailure::MissingCallable
    })?;
    let callable = index
        .callable_for_declaration(work.declaration)
        .ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "function body has no callable header declaration={:?} name={:?}",
                work.declaration,
                index.declaration_name(work.declaration),
            );
            CheckedBodyDriverFailure::MissingCallable
        })?;
    let context_count = usize::try_from(callable.shape.context_parameter_count)
        .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    if signature.parameters.len() != function.params.len() {
        crate::trace_compiler!(
            "fir",
            "function body parameter mismatch declaration={:?} name={} ast={} signature={} context={} receiver={:?}",
            work.declaration,
            function.name,
            function.params.len(),
            signature.parameters.len(),
            callable.shape.context_parameter_count,
            callable.shape.extension_receiver,
        );
        return Err(CheckedBodyDriverFailure::ParameterShapeMismatch);
    }
    let parameters = function
        .params
        .iter()
        .zip(signature.parameters.iter().copied())
        .map(|(parameter, ty)| CheckedBodyParameter {
            name: &parameter.name,
            ty,
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    // Defaults were checked and moved to `DefaultArgumentStore` during Pass 1. A declaration with
    // defaults but no ordinary body contributes no Pass-2 body fragment at all.
    if root.is_none() {
        return Ok(());
    }
    let context_receivers = signature.parameters.get(..context_count).ok_or_else(|| {
        crate::trace_compiler!(
            "fir",
            "function context parameter mismatch declaration={:?} name={} context={} signature={}",
            work.declaration,
            function.name,
            context_count,
            signature.parameters.len(),
        );
        CheckedBodyDriverFailure::ParameterShapeMismatch
    })?;
    let mut body = check_body_unit_with_parameters_and_defaults(
        file,
        info,
        source,
        work.owner,
        function.span,
        root,
        &parameters,
        &[],
        CheckedBodyReceiverShape {
            context_receivers,
            context_value_count: callable.shape.context_value_count,
            extension_receiver: body_extension_receiver(
                index,
                work.declaration,
                callable.shape.extension_receiver,
            ),
        },
        None,
        matches!(function.body, FunBody::Expr(_)).then_some(signature.result),
        index,
        origins,
        session,
    )
    .map_err(CheckedBodyDriverFailure::Check)?;
    publish_result_type(&mut body, signature.result)?;
    if matches!(function.body, FunBody::Expr(_)) {
        body.set_implicit_return();
    }
    session.absorb_checked_body(&body);
    dispatch_checked_body(callable, work, body, inline_bodies, ordinary_sink);
    Ok(())
}

/// Check signature-owned default expressions while the provider's Pass-1 syntax is live. The
/// provider supplies lexical ownership; signature, receiver shape, parameters, and emitted ABI all
/// come from the surviving target. The resulting checked fragment is stored before syntax dies.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_and_dispatch_signature_defaults_in_session(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: DefaultArgumentProvider,
    index: &ResolvedModuleIndex,
    active: &ActiveSourceDeclarations,
    origins: &mut OriginStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    let provider = index
        .declaration_anchor(work.provider)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    if provider.kind == DeclarationKind::Constructor {
        return check_and_dispatch_signature_constructor_defaults(
            file,
            info,
            source,
            work,
            index,
            active,
            origins,
            ordinary_sink,
            session,
        );
    }
    if provider.source != source || provider.kind != DeclarationKind::Function {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let function = active
        .function(file, work.provider)
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    let signature = index
        .signature(work.target)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let callable = index
        .callable_for_declaration(work.target)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    if signature.parameters.len() != function.params.len() {
        return Err(CheckedBodyDriverFailure::ParameterShapeMismatch);
    }
    let parameters = function
        .params
        .iter()
        .zip(signature.parameters.iter().copied())
        .map(|(parameter, ty)| CheckedBodyParameter {
            name: &parameter.name,
            ty,
            span: parameter.ty.span,
        })
        .collect::<Vec<_>>();
    let defaults = function
        .params
        .iter()
        .enumerate()
        .filter_map(|(parameter, value)| value.default.map(|expression| (parameter, expression)))
        .map(|(parameter, expression)| {
            Ok(CheckedBodyDefault {
                parameter: u32::try_from(parameter)
                    .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?,
                expression,
            })
        })
        .collect::<Result<Vec<_>, CheckedBodyDriverFailure>>()?;
    if defaults.is_empty() {
        return Err(CheckedBodyDriverFailure::MissingBody);
    }
    let context_count = usize::try_from(callable.shape.context_parameter_count)
        .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    let context_receivers = signature
        .parameters
        .get(..context_count)
        .ok_or(CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    let owner = BodyOwnerId::from_raw(work.target.raw());
    let mut body = check_body_unit_with_parameters_and_defaults(
        file,
        info,
        source,
        owner,
        function.span,
        None,
        &parameters,
        &defaults,
        CheckedBodyReceiverShape {
            context_receivers,
            context_value_count: callable.shape.context_value_count,
            extension_receiver: body_extension_receiver(
                index,
                work.target,
                callable.shape.extension_receiver,
            ),
        },
        None,
        None,
        index,
        origins,
        session,
    )
    .map_err(CheckedBodyDriverFailure::Check)?;
    publish_result_type(&mut body, signature.result)?;
    body.set_default_fragment();
    ordinary_sink.accept(owner, body);
    Ok(())
}

fn publish_result_type(
    body: &mut FirBody,
    result: crate::fir::ResolvedTy,
) -> Result<(), CheckedBodyDriverFailure> {
    match body.result_type() {
        Some(existing) if existing == result => Ok(()),
        Some(_) => Err(CheckedBodyDriverFailure::ParameterShapeMismatch),
        None => {
            body.set_result_type(result);
            Ok(())
        }
    }
}
