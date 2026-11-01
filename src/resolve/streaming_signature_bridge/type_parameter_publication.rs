//! Selection of stable type-parameter identities and bounds during signature publication.

use crate::ast::TypeRef;
use crate::libraries::GenericSig;
use crate::types::{Ty, TyKind};

use super::super::SymbolTable;

pub(super) fn publish_declared_counts(
    headers: &crate::fir::StreamedHeaderModule,
    index: &mut crate::fir::ResolvedModuleIndex,
) {
    for stub in &headers.stubs {
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let count = match declaration.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                type_parameters, ..
            }
            | crate::fir::HeaderDeclarationKind::Property {
                type_parameters, ..
            }
            | crate::fir::HeaderDeclarationKind::Classifier {
                type_parameters, ..
            }
            | crate::fir::HeaderDeclarationKind::TypeAlias {
                type_parameters, ..
            } => headers.syntax.type_parameters(type_parameters).len(),
            crate::fir::HeaderDeclarationKind::Constructor { .. } => 0,
        };
        index.publish_declaration_type_parameter_count(
            stub.id,
            u32::try_from(count).expect("too many declaration type parameters"),
        );
    }
}

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
            .map(|bound| match bound.kind() {
                TyKind::TyParam(next, upper_bound) | TyKind::Var(next, upper_bound) => {
                    parameter_reference(
                        next.as_str(),
                        upper_bound,
                        direct,
                        source_by_semantic,
                        visiting,
                        implicit,
                    )
                }
                _ => bound,
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
            if let TyKind::TyParam(semantic, _) | TyKind::Var(semantic, _) = bound.kind() {
                if let Some(source) = source_by_semantic.get(semantic.as_str()) {
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
        let primary = match primary.kind() {
            TyKind::TyParam(parameter, upper_bound) | TyKind::Var(parameter, upper_bound) => {
                parameter_reference(
                    parameter.as_str(),
                    upper_bound,
                    &direct,
                    &source_by_semantic,
                    &mut crate::fx_hash::HashSet::default(),
                    implicit,
                )
            }
            _ => primary,
        };
        let mut extra = declared.iter().copied().skip(1).collect::<Vec<_>>();
        for bound in declared {
            let (TyKind::TyParam(parameter, _) | TyKind::Var(parameter, _)) = bound.kind() else {
                continue;
            };
            let Some(source) = source_by_semantic.get(parameter.as_str()) else {
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
) -> crate::types::TyParamBindings {
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
                .entry(crate::types::TyParamName::from(name))
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
        .map(|formal| formal.as_str())
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
            if stable.len() == local.len() {
                Some(
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
                        .collect(),
                )
            } else {
                None
            }
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

pub(super) fn publish(
    index: &mut crate::fir::ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
    packed: &[crate::fir::HeaderTypeParameter],
    declared_names: &[String],
    declared_bounds: &[(String, crate::fir::HeaderTypeId, TypeRef)],
    symbolic: &super::super::TParams,
    stable: Option<&GenericSig>,
    table: &SymbolTable,
) -> Result<(), crate::fir::UnpublishableType> {
    let parameters = declared_names
        .iter()
        .zip(packed)
        .enumerate()
        .map(|(ordinal, (source_name, parameter))| {
            let semantic = symbolic.bound(source_name);
            let semantic_name = semantic_name(
                stable,
                ordinal,
                semantic.ty_param_name().unwrap_or(source_name),
            );
            let declared_bound_count = declared_bounds
                .iter()
                .filter(|(owner, _, _)| owner == source_name)
                .count();
            // `TParams` also carries inherited constraints after the written bounds. Publication
            // keeps exactly the written generic contract: kotlinc signs `T : U` as `T extends U`.
            let local_bounds = if declared_bound_count > 0 {
                let mut bounds = vec![semantic
                    .ty_param_bound()
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))];
                bounds.extend(
                    symbolic
                        .extra_bounds_of(source_name)
                        .into_iter()
                        .take(declared_bound_count - 1),
                );
                bounds
            } else {
                Vec::new()
            };
            (
                u32::try_from(ordinal).expect("too many declaration type parameters"),
                source_name.clone(),
                semantic_name.to_owned(),
                flags(parameter.flags),
                bounds(stable, ordinal, local_bounds, table),
            )
        })
        .collect::<Vec<_>>();
    index
        .publish_type_parameters_atomically(declaration, parameters)
        .map(drop)
}

/// Publish every source declaration's generic contract after nominal classifier headers exist.
/// Body-local internal declines transfer the whole contract to Pass 2; recorded source diagnostics
/// remain ordinary publication failures and continue to the next declaration.
pub(super) fn publish_source_declarations(
    headers: &crate::fir::StreamedHeaderModule,
    index: &mut crate::fir::ResolvedModuleIndex,
    semantics: &super::ProductionSignatureSemantics<'_>,
    solver_diagnostics: usize,
    deferred_candidates: &crate::fx_hash::HashSet<crate::fir::DeclarationId>,
    deferred_headers: &mut crate::fx_hash::HashSet<crate::fir::DeclarationId>,
    failed: &mut Vec<crate::fir::DeclarationId>,
    diagnostics: &mut crate::diag::DiagSink,
) {
    'declarations: for stub in &headers.stubs {
        if deferred_headers.contains(&stub.id)
            && stub.kind != crate::fir::DeclarationKind::Classifier
        {
            continue;
        }
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let (type_parameters, bounds, declaration_start) = match declaration.kind {
            crate::fir::HeaderDeclarationKind::Callable {
                type_parameters,
                bounds,
                signature_start,
                ..
            } => (type_parameters, Some(bounds), signature_start),
            crate::fir::HeaderDeclarationKind::Property {
                type_parameters,
                bounds,
                ..
            }
            | crate::fir::HeaderDeclarationKind::Classifier {
                type_parameters,
                bounds,
                ..
            } => (type_parameters, Some(bounds), stub.range.lo),
            crate::fir::HeaderDeclarationKind::TypeAlias {
                type_parameters, ..
            } => (type_parameters, None, stub.range.lo),
            crate::fir::HeaderDeclarationKind::Constructor { .. } => continue,
        };
        let packed = headers.syntax.type_parameters(type_parameters);
        if packed.is_empty() {
            continue;
        }
        let declared_names = packed
            .iter()
            .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_owned))
            .collect::<Option<Vec<_>>>();
        let Some(declared_names) = declared_names else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: type parameter name is not interned",
                stub.id,
            );
            super::publication_continuation::retain_or_report(
                diagnostics,
                &semantics.diagnostics.borrow()[solver_diagnostics..],
                stub.id,
                None,
                deferred_candidates,
                deferred_headers,
                failed,
            );
            continue 'declarations;
        };
        let declared_bounds = bounds
            .map(|bounds| headers.syntax.bounds(bounds))
            .unwrap_or_default()
            .iter()
            .map(|bound| {
                Some((
                    headers.lookup_names.get(bound.parameter)?.to_owned(),
                    bound.ty,
                    headers
                        .syntax
                        .transient_written_type_ref(bound.ty, &headers.lookup_names)?,
                ))
            })
            .collect::<Option<Vec<_>>>();
        let Some(declared_bounds) = declared_bounds else {
            crate::trace_compiler!(
                "fir",
                "signature finalization declined {:?}: type parameter bound is not interned",
                stub.id,
            );
            super::publication_continuation::retain_or_report(
                diagnostics,
                &semantics.diagnostics.borrow()[solver_diagnostics..],
                stub.id,
                None,
                deferred_candidates,
                deferred_headers,
                failed,
            );
            continue 'declarations;
        };
        let stable_generic = matches!(
            declaration.kind,
            crate::fir::HeaderDeclarationKind::Callable { .. }
        )
        .then(|| semantics.callable_signature(stub.id))
        .flatten()
        .and_then(|signature| signature.generic_sig.as_ref());
        let symbolic = match symbolic(
            index,
            stub.id,
            &declared_names,
            &declared_bounds,
            stable_generic.is_some(),
            semantics,
            stub.source,
            declaration_start,
        ) {
            Ok(symbolic) => symbolic,
            Err(diagnostic) => {
                super::publication_continuation::retain_or_report(
                    diagnostics,
                    &semantics.diagnostics.borrow()[solver_diagnostics..],
                    stub.id,
                    Some(diagnostic),
                    deferred_candidates,
                    deferred_headers,
                    failed,
                );
                continue 'declarations;
            }
        };
        if publish(
            index,
            stub.id,
            packed,
            &declared_names,
            &declared_bounds,
            &symbolic,
            stable_generic,
            semantics.table,
        )
        .is_err()
        {
            super::publication_continuation::retain_or_report(
                diagnostics,
                &semantics.diagnostics.borrow()[solver_diagnostics..],
                stub.id,
                None,
                deferred_candidates,
                deferred_headers,
                failed,
            );
        }
    }
}

fn mentions_external_formal(ty: Ty, own_formals: &[crate::types::TyParamName]) -> bool {
    match ty.kind() {
        // The occurrence identity is the declaration relation. Its inline bound is descriptive
        // metadata and may recursively mention other formals without adding a source edge here.
        TyKind::TyParam(name, _) | TyKind::Var(name, _) => !own_formals.contains(&name),
        TyKind::Fun(shape) => {
            shape
                .params
                .iter()
                .copied()
                .any(|parameter| mentions_external_formal(parameter, own_formals))
                || mentions_external_formal(shape.ret, own_formals)
        }
        TyKind::Nullable(inner)
        | TyKind::PlatformNullable(inner)
        | TyKind::MutabilityFlexible(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner) => mentions_external_formal(inner, own_formals),
        TyKind::Obj(_, arguments) => arguments
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
        let TyKind::Obj(interpreter, arguments) = bound.ty.get().kind() else {
            panic!("BaseTerminal.I must be bounded by Interpreter: {bound:?}");
        };
        assert!(interpreter.matches("Interpreter"));
        let [readable, _, self_parameter] = arguments.as_slice() else {
            panic!("Interpreter bound must retain its three exact arguments: {arguments:?}");
        };
        let (TyKind::StarProjection(readable), TyKind::TyParam(self_name, _)) =
            (readable.kind(), self_parameter.kind())
        else {
            panic!("Interpreter bound must retain its three exact arguments: {arguments:?}");
        };
        assert_eq!(self_name.as_str(), semantic);
        let TyKind::Obj(intermediary, parameters) = readable.kind() else {
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

    #[test]
    fn ordinary_generic_anonymous_and_local_overrides_publish_exact_pass_one_contracts() {
        let source = r#"
            abstract class Checker {
                abstract fun <T> run(value: T): T
                abstract fun <U> ping()
            }

            fun build() {
                val anonymous = object : Checker() {
                    override fun <T> run(value: T): T = value
                    override fun <U> ping() {}
                }
                class Local : Checker() {
                    override fun <T> run(value: T): T = value
                    override fun <U> ping() {}
                }
            }
        "#;
        let inputs = [SourceInput::kotlin(source).with_file_stem("GenericLocalOverrides")];
        let mut diagnostics = DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &inputs,
            crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &LangFeatures::new(),
            &mut diagnostics,
        );

        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let index = analysis
            .streamed
            .as_ref()
            .expect("ordinary generic local headers finalize in Pass 1")
            .module
            .index();
        let runs = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .filter(|declaration| index.declaration_name(*declaration) == Some("run"))
            .collect::<Vec<_>>();
        assert_eq!(runs.len(), 3, "base, anonymous, and named-local run");
        for run in runs {
            let formal = index.type_parameter(run, 0).expect("run<T> formal");
            let semantic = index
                .type_parameter_semantic_name(formal)
                .expect("stable run<T> semantic identity");
            let signature = index.signature(run).expect("Pass-1 run<T> signature");
            let [value] = signature.parameters.as_ref() else {
                panic!("run<T> has exactly one value parameter")
            };
            assert_eq!(value.get().ty_param_name(), Some(semantic));
            assert_eq!(signature.result.get().ty_param_name(), Some(semantic));
        }
        let pings = (0..index.declaration_count())
            .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
            .filter(|declaration| index.declaration_name(*declaration) == Some("ping"))
            .collect::<Vec<_>>();
        assert_eq!(pings.len(), 3, "base, anonymous, and named-local ping");
        for ping in pings {
            assert!(index.type_parameter(ping, 0).is_some(), "stable ping<U>");
            let signature = index
                .signature(ping)
                .expect("implicit-Unit ping<U> publishes in Pass 1");
            assert!(signature.parameters.is_empty());
            assert_eq!(signature.result.get(), Ty::Unit);
        }

        let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
        assert!(census.failures.is_empty(), "{:?}", census.failures);
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    }
}
