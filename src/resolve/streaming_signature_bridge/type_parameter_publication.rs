//! Selection of stable type-parameter identities and bounds during signature publication.

use crate::ast::TypeRef;
use crate::libraries::GenericSig;
use crate::types::Ty;

use super::super::SymbolTable;

/// Resolve a classifier type-parameter bound in the declaration's ordinary signature scope,
/// with this declaration's provisional semantic identities on the nearest classifier rung. The
/// ordinary type-reference path owns classifier arguments and their projected bounds; publication
/// must not rebuild a star as an unbounded `Any?` projection.
fn with_classifier_bound_scope<T>(
    semantics: &super::ProductionSignatureSemantics<'_>,
    scope: crate::fir::SignatureScope,
    provisional: &super::super::TParams,
    names: &[String],
    resolve: impl FnOnce(&super::super::CheckerScope<'_>) -> T,
) -> Result<T, crate::fir::DiagnosticId> {
    semantics.with_signature_type_scope(scope, |lexical| {
        let declaration =
            lexical.child(super::super::scope::ScopeKind::Function { receiver: None });
        declaration.declare_tparams(names, provisional, |_| false);
        resolve(&declaration)
    })
}

fn symbolic_from_resolved_bounds(
    declared_names: &[String],
    provisional: &super::super::TParams,
    resolved_bounds: &[(String, Ty)],
) -> super::super::TParams {
    let implicit = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
    let direct = resolved_bounds.iter().fold(
        crate::fx_hash::HashMap::<&str, Vec<Ty>>::default(),
        |mut direct, (owner, bound)| {
            direct.entry(owner).or_default().push(*bound);
            direct
        },
    );
    let source_by_semantic = declared_names
        .iter()
        .filter_map(|source| {
            provisional
                .bound(source)
                .ty_param_name()
                .map(|semantic| (semantic, source.as_str()))
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();

    fn parameter_reference(
        semantic: &'static str,
        resolved_upper_bound: Ty,
        direct: &crate::fx_hash::HashMap<&str, Vec<Ty>>,
        source_by_semantic: &crate::fx_hash::HashMap<&'static str, &str>,
        visiting: &mut crate::fx_hash::HashSet<&'static str>,
        implicit: Ty,
    ) -> Ty {
        let Some(source) = source_by_semantic.get(semantic) else {
            return Ty::ty_param(semantic, resolved_upper_bound);
        };
        if !visiting.insert(semantic) {
            return Ty::ty_param(semantic, implicit);
        }
        let bound = direct
            .get(source)
            .and_then(|bounds| bounds.first())
            .copied()
            .map(|bound| match bound {
                Ty::TyParam(next, upper_bound) => parameter_reference(
                    next,
                    *upper_bound,
                    direct,
                    source_by_semantic,
                    visiting,
                    implicit,
                ),
                bound => bound,
            })
            .unwrap_or(resolved_upper_bound);
        visiting.remove(semantic);
        Ty::ty_param(semantic, bound)
    }

    fn inherited_bounds(
        source: &str,
        direct: &crate::fx_hash::HashMap<&str, Vec<Ty>>,
        source_by_semantic: &crate::fx_hash::HashMap<&'static str, &str>,
        visiting: &mut crate::fx_hash::HashSet<String>,
        inherited: &mut Vec<Ty>,
    ) {
        if !visiting.insert(source.to_owned()) {
            return;
        }
        for bound in direct.get(source).into_iter().flatten() {
            if let Ty::TyParam(semantic, _) = *bound {
                if let Some(source) = source_by_semantic.get(semantic) {
                    inherited_bounds(source, direct, source_by_semantic, visiting, inherited);
                    continue;
                }
            }
            if !inherited.contains(bound) {
                inherited.push(*bound);
            }
        }
        visiting.remove(source);
    }

    let mut symbolic = super::super::TParams::default();
    for name in declared_names {
        let semantic = provisional
            .bound(name)
            .ty_param_name()
            .expect("provisional declaration type parameter must retain its semantic identity");
        let declared = direct.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        let primary = declared.first().copied().unwrap_or(implicit);
        let primary = match primary {
            Ty::TyParam(parameter, upper_bound) => parameter_reference(
                parameter,
                *upper_bound,
                &direct,
                &source_by_semantic,
                &mut crate::fx_hash::HashSet::default(),
                implicit,
            ),
            primary => primary,
        };
        let mut extra = declared.iter().copied().skip(1).collect::<Vec<_>>();
        for bound in declared {
            let Ty::TyParam(parameter, _) = *bound else {
                continue;
            };
            let Some(source) = source_by_semantic.get(parameter) else {
                continue;
            };
            let mut inherited = Vec::new();
            inherited_bounds(
                source,
                &direct,
                &source_by_semantic,
                &mut crate::fx_hash::HashSet::default(),
                &mut inherited,
            );
            for bound in inherited {
                if bound != primary && !extra.contains(&bound) {
                    extra.push(bound);
                }
            }
        }
        symbolic.insert_binding(name, Ty::ty_param(semantic, primary), extra);
    }
    symbolic
}

/// Type parameters visible from a declaration's enclosing owners, innermost first. A static nested
/// classifier does not inherit its classifier owner's parameters.
pub(super) fn enclosing(
    index: &crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
) -> crate::fx_hash::HashMap<String, Ty> {
    let mut visible = crate::fx_hash::HashMap::default();
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
    declared_bounds: &[(String, crate::fir::HeaderTypeId, TypeRef)],
    reuse_callable_contract: bool,
    semantics: &super::ProductionSignatureSemantics<'_>,
    source: crate::fir::SourceFileId,
    declaration_start: u32,
) -> Result<super::super::TParams, crate::fir::DiagnosticId> {
    let table = semantics.table;
    let enclosing = enclosing(index, declaration);
    // A bound is resolved through the declaration's own scope tower: its lexical classifiers,
    // their inherited nested classifiers and the file's package and imports. A classifier's bound
    // is in its header, which sees the classifier itself but not its body.
    let scope = crate::fir::SignatureScope {
        owner: declaration,
        source,
    };
    let semantic_bounds = declared_bounds
        .iter()
        .map(|(owner, _, bound)| (owner.clone(), bound.clone()))
        .collect::<Vec<_>>();
    let provisional = super::super::TParams::symbolic_from_decl_enclosing(
        declared_names,
        &semantic_bounds,
        &|name| semantics.qualified_type_classifier(scope, name),
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
        source.raw(),
        declaration_start,
    );
    // Callable collection already owns the stable semantic shape. Re-resolving a successful
    // F-bound through a provisional local parameter unfolds it once more, but skipping the written
    // bounds entirely also skips their diagnostics. Validate each source reference through the
    // ordinary header path, retain every diagnostic, and keep the authoritative callable shape.
    if reuse_callable_contract {
        let mut first_failure = None;
        for (_, syntax, bound) in declared_bounds {
            if let Err(diagnostic) = semantics.resolve_callable_header_bound(scope, *syntax, bound)
            {
                first_failure.get_or_insert(diagnostic);
            }
        }
        return first_failure.map_or(Ok(provisional), Err);
    }
    let resolved_bounds =
        with_classifier_bound_scope(semantics, scope, &provisional, declared_names, |lexical| {
            let mut first_failure = None;
            let mut resolved = Vec::with_capacity(declared_bounds.len());
            for (owner, syntax, bound) in declared_bounds {
                match semantics.resolve_classifier_header_bound(scope, lexical, *syntax, bound) {
                    Ok(bound) => resolved.push((owner.clone(), bound)),
                    Err(diagnostic) => {
                        first_failure.get_or_insert(diagnostic);
                    }
                }
            }
            first_failure.map_or(Ok(resolved), Err)
        })??;

    Ok(symbolic_from_resolved_bounds(
        declared_names,
        &provisional,
        &resolved_bounds,
    ))
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
            let is_interface =
                crate::fir::ResolvedTypeParameterBound::is_interface_type(bound, |owner| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::DiagSink;
    use crate::features::LangFeatures;
    use crate::libraries::EmptySymbolSource;
    use crate::source::SourceInput;

    #[test]
    fn recursive_declaration_bound_star_uses_the_classifier_formal_bound() {
        let source = r#"
            abstract class Interpreter<IS, TS, SELF>
                    where IS : Interpreter.Intermediary<SELF>,
                          TS : Interpreter.Terminal<SELF>,
                          SELF : Interpreter<IS, TS, SELF> {
                interface Intermediary<INTERPRETER>
                        where INTERPRETER : Interpreter<out Intermediary<INTERPRETER>, out Terminal<INTERPRETER>, INTERPRETER>
                interface Terminal<INTERPRETER>
                        where INTERPRETER : Interpreter<out Intermediary<INTERPRETER>, out Terminal<INTERPRETER>, INTERPRETER>
            }
            interface BaseTerminal<I : Interpreter<*, out BaseTerminal<I>, I>> : Interpreter.Terminal<I>
        "#;
        let inputs = [SourceInput::kotlin(source).with_file_stem("RecursiveBoundStar")];
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &inputs,
            crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        let index = analysis
            .streamed
            .as_ref()
            .expect("recursive declaration bounds must finalize in Pass 1")
            .module
            .index();
        let base_terminal = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .find(|declaration| {
                index
                    .classifier_header(*declaration)
                    .is_some_and(|classifier| classifier.classifier.matches("BaseTerminal"))
            })
            .expect("stable BaseTerminal classifier");
        let parameter = index
            .type_parameter(base_terminal, 0)
            .expect("BaseTerminal must publish I");
        let semantic = index
            .type_parameter_semantic_name(parameter)
            .expect("BaseTerminal.I must retain its semantic identity");
        let header = index
            .type_parameter_header(parameter)
            .expect("BaseTerminal.I must retain its bound");
        let [bound] = header.bounds.as_ref() else {
            panic!("BaseTerminal.I must have one bound: {:?}", header.bounds);
        };
        let Ty::Obj(interpreter, arguments) = bound.ty.get() else {
            panic!("BaseTerminal.I must be bounded by Interpreter: {bound:?}");
        };
        assert!(interpreter.matches("Interpreter"));
        let [Ty::StarProjection(readable), _, Ty::TyParam(self_name, _)] = arguments else {
            panic!("Interpreter bound must retain its three exact arguments: {arguments:?}");
        };
        assert_eq!(*self_name, semantic);
        let Ty::Obj(intermediary, parameters) = **readable else {
            panic!("Interpreter's first star must read as Intermediary<I>: {readable:?}");
        };
        assert!(intermediary.matches("Interpreter$Intermediary"));
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].ty_param_name(), Some(semantic));
    }

    #[test]
    fn resolved_bare_parameter_bound_keeps_the_inherited_intersection() {
        let implicit = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let t = crate::types::intern("test.T");
        let x = crate::types::intern("test.X");
        let a = Ty::obj("test/A");
        let b = Ty::obj("test/B");
        let mut provisional = super::super::super::TParams::default();
        provisional.insert_binding("T", Ty::ty_param(t, implicit), Vec::new());
        provisional.insert_binding("X", Ty::ty_param(x, implicit), Vec::new());
        let resolved = vec![
            ("T".to_string(), Ty::ty_param(x, implicit)),
            ("X".to_string(), a),
            ("X".to_string(), b),
        ];

        let symbolic = symbolic_from_resolved_bounds(
            &["T".to_string(), "X".to_string()],
            &provisional,
            &resolved,
        );

        assert_eq!(symbolic.bound("T"), Ty::ty_param(t, Ty::ty_param(x, a)));
        assert_eq!(symbolic.extra_bounds_of("T"), vec![a, b]);
        assert_eq!(symbolic.extra_bounds_of("X"), vec![b]);
    }

    #[test]
    fn resolved_enclosing_parameter_bound_keeps_its_resolved_upper_bound() {
        let implicit = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
        let own = crate::types::intern("test.S");
        let outer = crate::types::intern("test.OuterT");
        let outer_bound = Ty::obj("test/OuterBound");
        let mut provisional = super::super::super::TParams::default();
        provisional.insert_binding("S", Ty::ty_param(own, implicit), Vec::new());

        let symbolic = symbolic_from_resolved_bounds(
            &["S".to_string()],
            &provisional,
            &[("S".to_string(), Ty::ty_param(outer, outer_bound))],
        );

        assert_eq!(
            symbolic.bound("S"),
            Ty::ty_param(own, Ty::ty_param(outer, outer_bound))
        );
        assert!(symbolic.extra_bounds_of("S").is_empty());
    }
}
