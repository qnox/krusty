//! Applying the selected alias of an inner constructor to its enclosing receiver.

use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// Constructors named by one already-selected alias. Their callable signature belongs to the
/// alias's type parameters; the target declaration continues to own its physical invocation.
fn bound_inner_alias_constructor_candidates(
    source: &dyn SymbolSource,
    receiver: Ty,
    selected: &crate::symbol_resolver::ScopedClassifier,
) -> Vec<crate::libraries::FunctionInfo> {
    let Some(alias) = &selected.alias else {
        return Vec::new();
    };
    let Some(classifier) = source.classifier(selected.classifier) else {
        return Vec::new();
    };
    let Some(outer) = classifier.outer_instance else {
        return Vec::new();
    };
    let Some(context) = super::BoundInnerConstructorContext::new(source, receiver, &classifier)
    else {
        return Vec::new();
    };
    let own = classifier.own_type_parameter_count;
    let arguments = alias.expansion.type_args();
    if arguments.len() != classifier.type_params().len() {
        return Vec::new();
    }
    let expected_receiver = Ty::obj_args_name(outer, &arguments[own..]);
    let target = match apply_inner_alias_outer_in_source(source, alias.expansion, receiver) {
        InnerAliasOuterApplication::Applied(target) => target,
        InnerAliasOuterApplication::ReceiverMismatch => alias.expansion,
        InnerAliasOuterApplication::NotInner => return Vec::new(),
    };
    let substitutions = classifier
        .type_params()
        .iter()
        .cloned()
        .zip(target.type_args().iter().copied())
        .collect::<crate::symbol_resolver::GSigBinds>();
    let declaration_substitutions = classifier
        .type_params()
        .iter()
        .cloned()
        .zip(arguments.iter().copied())
        .collect::<crate::symbol_resolver::GSigBinds>();
    let bounds = alias
        .formals
        .iter()
        .map(|formal| {
            arguments
                .iter()
                .zip(classifier.type_param_bounds())
                .filter(|(argument, _)| argument.ty_param_name() == Some(formal.as_str()))
                .flat_map(|(_, bounds)| bounds)
                .map(|bound| crate::types::ty_subst_applied_arguments(*bound, &substitutions))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let formal_types = alias
        .formals
        .iter()
        .zip(&bounds)
        .map(|(formal, bounds)| {
            let bound = bounds
                .first()
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            (formal.clone(), Ty::ty_param(formal, bound))
        })
        .collect::<crate::symbol_resolver::GSigBinds>();
    classifier
        .constructors
        .iter()
        .filter_map(|constructor| {
            let original = crate::libraries::FunctionInfo::receiver_bound_constructor(
                selected.classifier,
                &classifier,
                constructor.clone(),
            )?;
            let original = original.semantic_signature();
            let mut underlying_signature = original.clone().into_owned();
            let rigid_formals = underlying_signature
                .formals
                .iter()
                .filter(|formal| !classifier.type_params().contains(formal))
                .cloned()
                .collect();
            underlying_signature.receiver = Some(expected_receiver);
            underlying_signature.params = original
                .params
                .iter()
                .map(|parameter| {
                    crate::types::ty_subst_applied_arguments(*parameter, &declaration_substitutions)
                })
                .collect();
            underlying_signature.formal_bounds = original
                .formal_bounds
                .iter()
                .map(|bounds| {
                    bounds
                        .iter()
                        .map(|bound| {
                            crate::types::ty_subst_applied_arguments(
                                *bound,
                                &declaration_substitutions,
                            )
                        })
                        .collect()
                })
                .collect();
            underlying_signature.ret = alias.expansion;
            let declaration_signature = crate::libraries::GenericSig {
                formals: alias.formals.clone(),
                formal_bounds: vec![Vec::new(); alias.formals.len()],
                receiver: Some(expected_receiver),
                params: original
                    .params
                    .iter()
                    .map(|parameter| {
                        crate::types::ty_subst_applied_arguments(
                            *parameter,
                            &declaration_substitutions,
                        )
                    })
                    .collect(),
                ret: alias.expansion,
                return_policy: crate::libraries::GenericReturnPolicy::Exact,
            };
            let mut candidate = context.candidate(
                source,
                receiver,
                selected.classifier,
                &classifier,
                constructor.clone(),
            )?;
            let parameters = candidate
                .semantic_params()
                .iter()
                .map(|parameter| {
                    crate::types::ty_subst_applied_arguments(*parameter, &substitutions)
                })
                .map(|parameter| crate::types::ty_subst_applied_arguments(parameter, &formal_types))
                .collect();
            let signature = crate::libraries::GenericSig {
                formals: alias.formals.clone(),
                formal_bounds: bounds.clone(),
                receiver: Some(crate::types::ty_subst_applied_arguments(
                    expected_receiver,
                    &formal_types,
                )),
                params: parameters,
                ret: crate::types::ty_subst_applied_arguments(alias.expansion, &formal_types),
                return_policy: crate::libraries::GenericReturnPolicy::Exact,
            };
            candidate.kind = crate::libraries::FnKind::Extension;
            candidate.receiver = signature.receiver;
            candidate.scope_rung = selected.scope_rung;
            candidate.scope_rank = selected.scope_rank;
            let declared = signature.parameters_with_receiver(candidate.context_count);
            candidate.callable.params = declared.to_vec();
            candidate.callable.declared_params = Some(declared);
            candidate.callable.ret = signature.ret;
            candidate.ret.class = Some(signature.ret);
            candidate.generic_sig = Some(signature);
            candidate.constructor_alias = Some(Box::new(crate::libraries::ConstructorAlias {
                declaration: alias.clone(),
                signature: declaration_signature,
                underlying_signature,
                rigid_formals,
            }));
            Some(candidate)
        })
        .collect()
}

impl crate::symbol_resolver::SymbolResolver<'_> {
    pub(crate) fn bound_inner_alias_constructor_candidates(
        &self,
        receiver: Ty,
        selected: &crate::symbol_resolver::ScopedClassifier,
    ) -> Vec<crate::libraries::FunctionInfo> {
        bound_inner_alias_constructor_candidates(&self.src, receiver, selected)
    }

    pub(crate) fn bound_inner_classifier(
        &self,
        receiver: Ty,
        name: &str,
    ) -> crate::symbol_resolver::InheritedNestedClassifier {
        bound_inner_classifier_in_source(&self.src, receiver, name)
    }
}

pub(crate) fn bound_inner_classifier_in_source(
    source: &dyn SymbolSource,
    receiver: Ty,
    name: &str,
) -> crate::symbol_resolver::InheritedNestedClassifier {
    use crate::symbol_resolver::InheritedNestedClassifier;
    let Some(root) = receiver.kotlin_class_internal() else {
        return InheritedNestedClassifier::NotFound;
    };
    let owners = crate::symbol_resolver::applied_hierarchy(source, receiver)
        .into_iter()
        .map(|(owner, _, _)| owner)
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut level = vec![root];
    let mut seen = crate::fx_hash::HashSet::default();
    while !level.is_empty() {
        let mut matches = crate::fx_hash::HashSet::default();
        let mut next = Vec::new();
        for owner in level {
            if !seen.insert(owner) {
                continue;
            }
            let symbols = source.symbols(
                crate::symbol_source::SymbolNamespace::Classifier(owner),
                name,
            );
            if let Some(classifier) = symbols.classifier_name.filter(|_| {
                symbols
                    .classifier
                    .as_ref()
                    .and_then(|shape| shape.outer_instance)
                    .is_some_and(|outer| owners.contains(&outer))
            }) {
                matches.insert(classifier);
            }
            next.extend(
                crate::symbol_resolver::direct_supertypes(source, Ty::obj_name(owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal),
            );
        }
        match matches.len() {
            0 => level = next,
            1 => {
                return InheritedNestedClassifier::Found(
                    matches.into_iter().next().expect("one inner classifier"),
                )
            }
            _ => return InheritedNestedClassifier::Ambiguous,
        }
    }
    InheritedNestedClassifier::NotFound
}

pub(crate) enum InnerAliasOuterApplication {
    Applied(Ty),
    ReceiverMismatch,
    NotInner,
}

pub(crate) fn apply_inner_alias_outer_in_source(
    source: &dyn SymbolSource,
    target: Ty,
    receiver: Ty,
) -> InnerAliasOuterApplication {
    let Some(internal) = target.kotlin_class_internal() else {
        return InnerAliasOuterApplication::NotInner;
    };
    let Some(classifier) = source.classifier(internal) else {
        return InnerAliasOuterApplication::NotInner;
    };
    let Some(outer_owner) = classifier.outer_instance else {
        return InnerAliasOuterApplication::NotInner;
    };
    let Some(applied_outer) = crate::symbol_resolver::applied_hierarchy(
        source,
        crate::symbol_resolver::member_scope_receiver(receiver),
    )
    .into_iter()
    .find_map(|(owner, applied, _)| (owner == outer_owner).then_some(applied)) else {
        return InnerAliasOuterApplication::ReceiverMismatch;
    };
    let own_count = classifier
        .own_type_parameter_count
        .min(classifier.type_params().len());
    let captured_count = classifier.type_params().len().saturating_sub(own_count);
    let arguments = target.type_args();
    if arguments.len() < own_count + captured_count {
        return InnerAliasOuterApplication::ReceiverMismatch;
    }
    let expected_outer = Ty::obj_args_name(
        outer_owner,
        &arguments[own_count..own_count + captured_count],
    );
    let mut bindings = crate::symbol_resolver::GSigBinds::default();
    crate::symbol_resolver::unify_ty_from_symbols(
        source,
        expected_outer,
        applied_outer,
        &mut bindings,
    );
    let expected_outer = crate::symbol_resolver::ty_subst_keep_unbound(expected_outer, &bindings);
    let oracle = crate::symbol_resolver::SourceOracle(source);
    if !crate::assignable::is_assignable(
        &crate::assignable::TyCtx::new(),
        &oracle,
        applied_outer,
        expected_outer,
    ) {
        return InnerAliasOuterApplication::ReceiverMismatch;
    }
    let target = crate::symbol_resolver::ty_subst_keep_unbound(target, &bindings);
    InnerAliasOuterApplication::Applied(apply_inner_classifier_outer_in_source(
        source, target, receiver,
    ))
}

pub(crate) fn apply_inner_classifier_outer_in_source(
    source: &dyn SymbolSource,
    target: Ty,
    outer: Ty,
) -> Ty {
    let Some(internal) = target.kotlin_class_internal() else {
        return target;
    };
    let Some(classifier) = source.classifier(internal) else {
        return target;
    };
    let Some(outer_owner) = classifier.outer_instance else {
        return target;
    };
    let Some(applied_outer) = crate::symbol_resolver::applied_hierarchy(
        source,
        crate::symbol_resolver::member_scope_receiver(outer),
    )
    .into_iter()
    .find_map(|(owner, applied, _)| (owner == outer_owner).then_some(applied)) else {
        return target;
    };
    let own_count = classifier
        .own_type_parameter_count
        .min(classifier.type_params().len());
    let captured_count = classifier.type_params().len().saturating_sub(own_count);
    if applied_outer.type_args().len() < captured_count {
        return target;
    }
    let mut arguments = target
        .type_args()
        .iter()
        .copied()
        .take(own_count)
        .collect::<Vec<_>>();
    for index in arguments.len()..own_count {
        let formal = &classifier.type_params()[index];
        let bound = classifier
            .type_param_bounds()
            .get(index)
            .and_then(|bounds| bounds.first())
            .copied()
            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
        arguments.push(Ty::ty_param(formal, bound));
    }
    arguments.extend(
        applied_outer
            .type_args()
            .iter()
            .copied()
            .take(captured_count),
    );
    Ty::obj_args_name(internal, &arguments)
}
