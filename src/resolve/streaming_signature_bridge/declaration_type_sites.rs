//! Structural binding of temporary graph types to compact declaration-header sites.

fn bind_expression(
    graph: &mut crate::fir::SignatureGraph,
    headers: &crate::fir::StreamedHeaderModule,
    expression: crate::fir::SigExprId,
    declaration_syntax: crate::fir::HeaderTypeId,
) {
    // Selected spelling facts belong to the written occurrence. The compact declaration arena
    // may retain an expanded alias template at this role, but metadata later walks its written
    // tree. Binding the graph expression to the expanded id leaves a valid semantic signature
    // with no exact spelling record (for example, `Cargo` expanded to `String`).
    let declaration_syntax = headers.syntax.written_type_id(declaration_syntax);
    let Some(graph_syntax) = graph.expression_type_syntax(expression) else {
        graph.bind_declaration_type_syntax(expression, declaration_syntax);
        return;
    };
    let mut pending = vec![(graph_syntax, declaration_syntax)];
    while let Some((graph_syntax, declaration_syntax)) = pending.pop() {
        let Some((_, graph_function, graph_children)) = graph.written_type_node(graph_syntax)
        else {
            return;
        };
        let Some((_, declaration_function, declaration_children)) =
            headers.syntax.written_type_node(declaration_syntax)
        else {
            return;
        };
        if graph_function != declaration_function
            || graph_children.len() != declaration_children.len()
        {
            return;
        }
        pending.extend(graph_children.into_iter().zip(declaration_children));
    }
    // Publish the role edge only after its complete written shape has been validated. A partial
    // root edge would let later spelling publication hide a missing child fact by treating another
    // declaration role as canonical.
    graph.bind_declaration_type_syntax(expression, declaration_syntax);
    let Some(arguments) = graph.written_type_application_arguments(expression) else {
        return;
    };
    let arguments = arguments.collect::<Vec<_>>();
    let declaration_written = headers.syntax.written_type_id(declaration_syntax);
    let Some((_, _, children)) = headers.syntax.written_type_node(declaration_written) else {
        return;
    };
    for (argument, index) in arguments {
        if let Some(child) = children.get(index as usize).copied() {
            bind_expression(graph, headers, argument, child);
        }
    }
}

/// Join syntax by declaration role and ordinal. Constructor graph parameters are only ordinary
/// constructor parameters; context parameters stay on the owning classifier/header path.
pub(super) fn bind_graph_declaration_types(
    graph: &mut crate::fir::SignatureGraph,
    headers: &crate::fir::StreamedHeaderModule,
) {
    let mut bindings = Vec::new();
    for stub in &headers.stubs {
        let Some(explicit) = graph.explicit_signature_types(stub.id) else {
            continue;
        };
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let (parameters, result, getter_result, receiver, storage) = match declaration.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                parameters,
                result,
                receiver,
                ..
            } => (
                headers
                    .syntax
                    .parameters(parameters)
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect::<Vec<_>>(),
                match result {
                    crate::fir::HeaderResultType::Explicit(result) => Some(result),
                    crate::fir::HeaderResultType::ImplicitUnit
                    | crate::fir::HeaderResultType::Inferred => None,
                },
                None,
                receiver,
                None,
            ),
            crate::fir::HeaderDeclarationKind::Property {
                context_parameters,
                declared_type,
                getter_type,
                receiver,
                backing_field_type,
                ..
            } => (
                headers
                    .syntax
                    .parameters(context_parameters)
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect(),
                declared_type.or(getter_type),
                declared_type.and(getter_type),
                receiver,
                backing_field_type,
            ),
            crate::fir::HeaderDeclarationKind::Constructor { parameters, .. } => (
                headers
                    .syntax
                    .parameters(parameters)
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect(),
                None,
                None,
                None,
                None,
            ),
            crate::fir::HeaderDeclarationKind::Classifier { .. }
            | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => continue,
        };
        let expressions = graph.operands(explicit.parameters).to_vec();
        if expressions.len() == parameters.len() {
            bindings.extend(expressions.into_iter().zip(parameters));
        }
        bindings.extend(explicit.result.zip(result));
        bindings.extend(explicit.getter_result.zip(getter_result));
        bindings.extend(explicit.receiver.zip(receiver));
        bindings.extend(explicit.storage.zip(storage));
        if stub.kind == crate::fir::DeclarationKind::Property
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::PROPERTY_PARAMETER)
        {
            let shared_primary_parameter =
                headers.declarations.anchor(stub.id).and_then(|anchor| {
                    let owner = anchor.owner?;
                    let declaration = headers.syntax.declaration(owner)?;
                    let crate::fir::HeaderDeclarationKind::Classifier {
                        primary_parameters, ..
                    } = declaration.kind
                    else {
                        return None;
                    };
                    headers
                        .syntax
                        .parameters(primary_parameters)
                        .get(anchor.sibling as usize)
                        .map(|parameter| parameter.ty)
                });
            bindings.extend(explicit.result.zip(shared_primary_parameter));
        }
    }
    let classifier_parents = graph.explicit_classifier_parents().collect::<Vec<_>>();
    for (declaration, explicit) in classifier_parents {
        let Some(crate::fir::HeaderDeclarationKind::Classifier {
            base, supertypes, ..
        }) = headers
            .syntax
            .declaration(declaration)
            .map(|header| header.kind)
        else {
            continue;
        };
        bindings.extend(explicit.superclass.zip(base));
        let expressions = graph.operands(explicit.supertypes);
        let syntax = headers.syntax.type_operands(supertypes);
        if expressions.len() == syntax.len() {
            bindings.extend(expressions.iter().copied().zip(syntax.iter().copied()));
        }
    }
    for (expression, declaration_syntax) in bindings {
        bind_expression(graph, headers, expression, declaration_syntax);
    }
}
