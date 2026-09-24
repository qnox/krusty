//! Selection of stable type-parameter identities and bounds during signature publication.

use crate::ast::TypeRef;
use crate::libraries::GenericSig;
use crate::types::Ty;

use super::super::SymbolTable;

/// Type parameters visible from a declaration's enclosing owners, innermost first. A static nested
/// classifier does not inherit its classifier owner's parameters.
pub(super) fn enclosing(
    index: &crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
) -> std::collections::HashMap<String, Ty> {
    let mut visible = std::collections::HashMap::new();
    let mut scope = declaration;
    while let Some(header) = index.declaration_header(scope) {
        let Some(current) = header.owner else {
            break;
        };
        let static_nested = header.kind == crate::fir::DeclarationKind::Classifier
            && !header.flags.has(crate::fir::DeclarationFlags::INNER)
            && index.classifier_header(current).is_some();
        if static_nested {
            break;
        }
        for ordinal in 0.. {
            let Some(parameter) = index.type_parameter(current, ordinal) else {
                break;
            };
            let (Some(name), Some(semantic), Some(header)) = (
                index.type_parameter_name(parameter),
                index.type_parameter_semantic_name(parameter),
                index.type_parameter_header(parameter),
            ) else {
                continue;
            };
            let bound = header.bounds.first().map_or_else(
                || Ty::nullable(Ty::obj("kotlin/Any")),
                |bound| bound.ty.get(),
            );
            visible
                .entry(name.to_owned())
                .or_insert_with(|| Ty::ty_param(semantic, bound));
        }
        scope = current;
    }
    visible
}

pub(super) fn symbolic(
    index: &crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    declared_names: &[String],
    declared_bounds: &[(String, TypeRef)],
    table: &SymbolTable,
    source: u32,
    declaration_start: u32,
) -> super::super::TParams {
    let enclosing = enclosing(index, declaration);
    super::super::TParams::symbolic_from_decl_enclosing(
        declared_names,
        declared_bounds,
        &|name| table.class_names.get(name),
        &|name| {
            // This declaration's own parameter shadows an enclosing parameter with the same name.
            (!declared_names.iter().any(|declared| declared == name))
                .then(|| enclosing.get(name).copied())
                .flatten()
        },
    )
    .alpha_renamed_declaration(
        declared_names,
        table.compilation_id,
        source,
        declaration_start,
    )
}

pub(super) fn semantic_name<'a>(
    stable: Option<&'a GenericSig>,
    ordinal: usize,
    local: &'a str,
) -> &'a str {
    stable
        .and_then(|generic| generic.formals.get(ordinal))
        .map(String::as_str)
        .unwrap_or(local)
}

pub(super) fn bounds(
    stable: Option<&GenericSig>,
    ordinal: usize,
    local: Vec<Ty>,
    table: &SymbolTable,
) -> Vec<(Ty, bool)> {
    let bounds = stable
        .and_then(|generic| {
            let stable = generic.formal_bounds.get(ordinal)?;
            (stable.len() == local.len()).then(|| {
                stable
                    .iter()
                    .copied()
                    .zip(&local)
                    .map(|(stable, &local)| {
                        if mentions_external_formal(stable, &generic.formals) {
                            stable
                        } else {
                            local
                        }
                    })
                    .collect()
            })
        })
        .unwrap_or(local);
    bounds
        .into_iter()
        .map(|bound| {
            let is_interface = bound.non_null().obj_internal().is_some_and(|owner| {
                table
                    .classes
                    .get(&owner)
                    .is_some_and(|classifier| classifier.is_interface())
                    || table
                        .libraries
                        .classifier(owner)
                        .is_some_and(|classifier| classifier.is_interface())
            });
            (bound, is_interface)
        })
        .collect()
}

pub(super) fn flags(
    flags: crate::fir::HeaderTypeParameterFlags,
) -> crate::fir::ResolvedTypeParameterFlags {
    let variance = if flags.is_in() {
        crate::types::TypeVariance::In
    } else if flags.is_out() {
        crate::types::TypeVariance::Out
    } else {
        crate::types::TypeVariance::Invariant
    };
    crate::fir::ResolvedTypeParameterFlags::new(variance, flags.is_non_null(), flags.is_reified())
}

fn mentions_external_formal(ty: Ty, own_formals: &[String]) -> bool {
    match ty {
        // The occurrence identity is the declaration relation. Its inline bound is descriptive
        // metadata and may recursively mention other formals without adding a source edge here.
        Ty::TyParam(name, _) => !own_formals.iter().any(|formal| formal == name),
        Ty::Fun(shape) => {
            shape
                .params
                .iter()
                .copied()
                .any(|parameter| mentions_external_formal(parameter, own_formals))
                || mentions_external_formal(shape.ret, own_formals)
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::StarProjection(inner) => mentions_external_formal(*inner, own_formals),
        Ty::Obj(_, arguments) => arguments
            .iter()
            .copied()
            .any(|argument| mentions_external_formal(argument, own_formals)),
        _ => false,
    }
}
