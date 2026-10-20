//! Star-projection bounds reapplied to compact header types at signature publication.

use super::*;

/// Reapply a compact header's star-projection syntax to a provisionally resolved legacy type.
///
/// The legacy collector has no classifier-shape input while resolving a `TypeRef`, so it represents
/// every `*` as `out Any?`. The compact header still owns the exact star bit and the
/// provider-normalized classifier (module or classpath) owns the target parameter bound. Combine
/// those stable Pass-1 facts at publication time; an explicitly written `out Any?` has no star bit
/// and is therefore never rewritten.
pub(super) fn compact_header_star_bounds(
    table: &SymbolTable,
    syntax: &TypeRef,
    resolved: Ty,
) -> Option<Ty> {
    Some(match resolved.kind() {
        TyKind::Nullable(inner) => Ty::nullable(compact_header_star_bounds(table, syntax, inner)?),
        TyKind::PlatformNullable(inner) => {
            Ty::platform_nullable(compact_header_star_bounds(table, syntax, inner)?)
        }
        TyKind::InProjection(inner) if !syntax.is_star_projection() => {
            Ty::in_projection(compact_header_star_bounds(table, syntax, inner)?)
        }
        TyKind::OutProjection(inner) if !syntax.is_star_projection() => {
            Ty::out_projection(compact_header_star_bounds(table, syntax, inner)?)
        }
        TyKind::Obj(owner, resolved_arguments) if !syntax.targs.is_empty() => {
            // Read the formal bounds from the provider-normalized declaration, so a classpath
            // classifier's `*` gets its declared bound exactly as a module classifier's does.
            let module = crate::module_symbols::ModuleSymbols::new(table);
            let source = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                &*table.libraries as &dyn crate::symbol_source::SymbolSource,
            ]);
            let classifier = crate::symbol_source::SymbolSource::classifier(&source, owner)?;
            let mut arguments = resolved_arguments.to_vec();
            for index in 0..syntax.targs.len().min(arguments.len()) {
                let argument_syntax = &syntax.targs[index];
                if !argument_syntax.is_star_projection() {
                    arguments[index] =
                        compact_header_star_bounds(table, argument_syntax, arguments[index])?;
                    continue;
                }
                let bindings = classifier
                    .type_params()
                    .iter()
                    .zip(arguments.iter())
                    .enumerate()
                    .filter_map(|(ordinal, (formal, actual))| {
                        (ordinal != index).then_some((formal.clone(), *actual))
                    })
                    .collect::<crate::symbol_resolver::GSigBinds>();
                let upper_bound = classifier
                    .type_param_bounds()
                    .get(index)
                    .and_then(|bounds| bounds.first())
                    .copied()
                    .map(|bound| crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings))
                    .unwrap_or_else(|| Ty::nullable(Ty::obj_name(crate::types::wk::any())));
                arguments[index] = Ty::star_projection(upper_bound);
            }
            Ty::obj_args_name(owner, &arguments)
        }
        TyKind::Fun(function) if syntax.name == "<fun>" => {
            let parameters = syntax
                .fun_params
                .iter()
                .zip(function.params.iter())
                .map(|(syntax, resolved)| compact_header_star_bounds(table, syntax, *resolved))
                .collect::<Option<Vec<_>>>()?;
            let result = syntax
                .arg
                .as_deref()
                .map(|syntax| compact_header_star_bounds(table, syntax, function.ret))
                .unwrap_or(Some(function.ret))?;
            Ty::fun_with_shape(
                parameters,
                result,
                function.context_count,
                function.has_receiver,
                function.suspend,
            )
        }
        _ => resolved,
    })
}

/// Reapply star bounds to one source value-parameter type without confusing the source element
/// annotation with the semantic array slot introduced by `vararg`.
pub(super) fn compact_header_value_parameter_star_bounds(
    table: &SymbolTable,
    syntax: &TypeRef,
    resolved: Ty,
    is_vararg: bool,
) -> Option<Ty> {
    if !is_vararg {
        return compact_header_star_bounds(table, syntax, resolved);
    }
    let element = resolved.array_elem()?;
    Some(crate::types::semantic_value_parameter_ty(
        compact_header_star_bounds(table, syntax, element)?,
        true,
    ))
}
