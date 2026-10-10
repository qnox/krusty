//! Constructors of an `inner` classifier reached through a value receiver.
//!
//! Kotlin places the constructors of an inner classifier in the member level of the receiver that
//! supplies its outer instance: `outer.Inner(args)`, `outer?.Inner(args)`, and a bare
//! `Inner(args)` resolved on an implicit receiver. When that receiver also declares same-named
//! member functions, both families form one candidate list. Applicability and most-specific
//! selection run once over it, an ambiguity between a function and a constructor is reported as
//! such, and the constructor path only materializes the declaration that selection chose.

use super::*;

/// The inner classifier a receiver binds for one call spelling.
enum BoundInnerClassifier {
    NotFound,
    Ambiguous,
    Found {
        internal: TypeName,
        /// The in-scope typealias application that named the classifier, when one did.
        alias_target: Option<Ty>,
    },
}

pub(super) enum InnerAliasOuterApplication {
    Applied(Ty),
    ReceiverMismatch,
    NotInner,
}

/// One receiver-bound construction being committed.
#[derive(Clone, Copy)]
pub(super) struct BoundInnerConstruction<'a> {
    pub(super) call_args: CallArgs<'a>,
    /// The explicit receiver expression that supplies the outer instance; an implicit receiver is
    /// recorded by the caller's receiver selection instead.
    pub(super) explicit_outer: Option<ExprId>,
    pub(super) receiver: Ty,
    pub(super) arg_names: Option<&'a [Option<String>]>,
    pub(super) expected: Option<Ty>,
}

impl Checker<'_> {
    /// Classifier constructor denoted by `receiver.name(...)` when the classifier captures that
    /// receiver as its enclosing instance, selected over the constructor family alone. A receiver
    /// that also declares same-named member functions selects through the member tower rung
    /// instead, where both families are one candidate list.
    ///
    /// `None` when no inner classifier matches, so the caller continues its own tower.
    pub(super) fn bound_inner_constructor_call(
        &mut self,
        scope: &CheckerScope<'_>,
        construction: BoundInnerConstruction<'_>,
        name: &str,
    ) -> Option<Ty> {
        let call = construction.call_args.call;
        match self.bound_inner_classifier(
            scope,
            call,
            construction.receiver,
            name,
            construction.expected,
        ) {
            BoundInnerClassifier::Found {
                internal,
                alias_target,
            } => self.commit_bound_inner_constructor(
                scope,
                construction,
                internal,
                alias_target,
                None,
            ),
            BoundInnerClassifier::Ambiguous => {
                self.report_ambiguous_inner_classifier(call, name);
                Some(Ty::Error)
            }
            BoundInnerClassifier::NotFound => None,
        }
    }

    pub(super) fn report_ambiguous_inner_classifier(&mut self, call: ExprId, name: &str) {
        self.diags.error(
            self.call_callee_name_span(call),
            format!("overload resolution ambiguity for inner classifier '{name}'"),
        );
    }

    /// The inner classifier `receiver.name` binds. Direct nested classifiers are searched by
    /// hierarchy level; an in-scope typealias to an inner classifier participates only when no
    /// member classifier wins. Every edge comes from the federated symbol record—no internal name
    /// is manufactured from the source spelling.
    fn bound_inner_classifier(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        receiver: Ty,
        name: &str,
        expected: Option<Ty>,
    ) -> BoundInnerClassifier {
        match self.bound_inner_constructor_classifier(receiver, name) {
            InheritedNestedClassifier::Found(internal) => BoundInnerClassifier::Found {
                internal,
                alias_target: None,
            },
            InheritedNestedClassifier::Ambiguous => BoundInnerClassifier::Ambiguous,
            InheritedNestedClassifier::NotFound => {
                let target = self.scoped_source_alias_call_ty(scope, call, name, expected);
                match target.map(|target| self.apply_inner_alias_outer(target, receiver)) {
                    Some(InnerAliasOuterApplication::Applied(target)) => {
                        BoundInnerClassifier::Found {
                            internal: target
                                .kotlin_class_internal()
                                .expect("an applied inner alias keeps its classifier"),
                            alias_target: Some(target),
                        }
                    }
                    Some(InnerAliasOuterApplication::ReceiverMismatch) => {
                        BoundInnerClassifier::NotFound
                    }
                    Some(InnerAliasOuterApplication::NotInner) | None => {
                        BoundInnerClassifier::NotFound
                    }
                }
            }
        }
    }

    /// Apply the receiver's exact outer-class application to an alias of an inner classifier.
    /// The alias template remains authoritative: fixed captured arguments must accept the receiver,
    /// while alias formals are inferred from it and substituted into every occurrence (including
    /// the inner classifier's own arguments).
    pub(super) fn apply_inner_alias_outer(
        &self,
        target: Ty,
        receiver: Ty,
    ) -> InnerAliasOuterApplication {
        let Some(internal) = target.kotlin_class_internal() else {
            return InnerAliasOuterApplication::NotInner;
        };
        let source = self.fed_source();
        let Some(classifier) = source.classifier(internal) else {
            return InnerAliasOuterApplication::NotInner;
        };
        let Some(outer_owner) = classifier.outer_instance else {
            return InnerAliasOuterApplication::NotInner;
        };
        let Some(applied_outer) = crate::symbol_resolver::applied_hierarchy(
            &source,
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
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        crate::symbol_resolver::unify_ty_from_symbols(
            &source,
            expected_outer,
            applied_outer,
            &mut bindings,
        );
        let expected_outer =
            crate::symbol_resolver::ty_subst_keep_unbound(expected_outer, &bindings);
        if !self.receiver_is_assignable(applied_outer, expected_outer) {
            return InnerAliasOuterApplication::ReceiverMismatch;
        }
        let target = crate::types::ty_subst_alias_expansion(target, &bindings);
        InnerAliasOuterApplication::Applied(self.apply_inner_classifier_outer(target, receiver))
    }

    /// Report kotlinc's declaration-shaped receiver mismatch when one alias constructor is the
    /// rejected candidate. With several constructors the ordinary unresolved-reference path owns
    /// the terminal diagnostic until overload diagnostics can list that whole family exactly.
    pub(super) fn report_inner_alias_receiver_mismatch(
        &mut self,
        expression: ExprId,
        name: &str,
        target: Ty,
    ) -> bool {
        let Some(internal) = target.kotlin_class_internal() else {
            return false;
        };
        let Some(classifier) = self.resolved_type_name(internal) else {
            return false;
        };
        let mut constructors = self.constructor_declarations(internal, &classifier);
        if constructors.len() != 1 {
            return false;
        }
        let constructor = constructors.pop().expect("one constructor");
        let parameters =
            Self::access_parameter_display(&constructor.params, &constructor.call_sig.param_names);
        if !self.silent_error_exprs.insert(expression) {
            return true;
        }
        self.diags.error(
            self.member_name_span(expression, name),
            format!(
                "candidate 'constructor({parameters}): {}' is inapplicable because of a receiver type mismatch.",
                target.source_name(),
            ),
        );
        true
    }

    /// Apply the selected enclosing classifier to an inner classifier token. The stable type layout
    /// stores the inner declaration's own parameters first and its captured outer parameters next.
    /// A nested constructor reference has no runtime receiver expression, but its qualified LHS
    /// still fixes the type of the leading outer parameter (`Foo<String>::Inner`).
    pub(super) fn apply_inner_classifier_outer(&self, target: Ty, outer: Ty) -> Ty {
        let Some(internal) = target.kotlin_class_internal() else {
            return target;
        };
        let source = self.fed_source();
        let Some(classifier) = source.classifier(internal) else {
            return target;
        };
        let Some(outer_owner) = classifier.outer_instance else {
            return target;
        };
        let Some(applied_outer) = crate::symbol_resolver::applied_hierarchy(
            &source,
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

    /// The constructors of `internal` as member candidates of `receiver`, from the same
    /// declaration set the constructor path materializes.
    pub(super) fn bound_inner_constructor_candidates(
        &self,
        receiver: Ty,
        internal: TypeName,
    ) -> Vec<crate::libraries::FunctionInfo> {
        let Some(classifier) = self.resolved_type_name(internal) else {
            return Vec::new();
        };
        let declarations = self.constructor_declarations(internal, &classifier);
        crate::symbol_resolver::bound_inner_constructor_candidates(
            &self.fed_source(),
            receiver,
            internal,
            &classifier,
            declarations,
        )
    }

    /// The constructors of the inner classifier that shares `receiver`'s member level with its
    /// same-named member functions, for contextual lambda shaping ahead of selection. The caller
    /// asks only when such member functions exist; kotlinc lists the constructors first.
    pub(super) fn member_level_constructor_candidates(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Vec<crate::libraries::FunctionInfo> {
        match self.member_level_inner_classifier(receiver, name) {
            InheritedNestedClassifier::Found(internal) => {
                self.bound_inner_constructor_candidates(receiver, internal)
            }
            InheritedNestedClassifier::NotFound | InheritedNestedClassifier::Ambiguous => {
                Vec::new()
            }
        }
    }

    /// Whether `receiver`'s member level holds both same-named member functions and the
    /// constructors of `internal`, so that one selection over that level decides a bare call.
    pub(super) fn receiver_member_level_owns_constructors(
        &self,
        receiver: Ty,
        name: &str,
        internal: TypeName,
    ) -> bool {
        self.member_level_inner_classifier(receiver, name)
            == InheritedNestedClassifier::Found(internal)
            && !self
                .body_local_member_overload_rung(receiver, name)
                .1
                .is_empty()
    }

    /// Constructor declarations of one classifier: its provider record, with a body-local
    /// classifier's checked constructors replacing their provisional counterparts.
    pub(super) fn constructor_declarations(
        &self,
        internal: TypeName,
        classifier: &crate::libraries::LibraryType,
    ) -> Vec<crate::libraries::LibraryMember> {
        let checked_local = self.checked_local_constructors.get(&internal);
        let mut declarations = classifier
            .constructors
            .iter()
            .filter(|declaration| {
                !checked_local.is_some_and(|checked| {
                    declaration.stable_declaration.is_some_and(|stable| {
                        checked
                            .iter()
                            .any(|candidate| candidate.stable_declaration == Some(stable))
                    })
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        declarations.extend(checked_local.into_iter().flatten().cloned());
        declarations
    }

    /// Commit the construction of `internal` with the receiver as its outer instance. `selected`
    /// is the constructor a wider member-level selection already chose; without it the
    /// constructor family is selected here.
    fn commit_bound_inner_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        construction: BoundInnerConstruction<'_>,
        internal: TypeName,
        alias_target: Option<Ty>,
        selected: Option<&crate::libraries::FunctionInfo>,
    ) -> Option<Ty> {
        let BoundInnerConstruction {
            call_args: CallArgs { call, args, .. },
            explicit_outer,
            receiver,
            arg_names,
            expected,
        } = construction;
        crate::trace_compiler!(
            "resolve",
            "bound inner constructor call={call:?} outer={receiver:?} classifier={} preselected={}",
            internal.render(),
            selected.is_some(),
        );
        match self.record_resolved_library_constructor(
            scope,
            call,
            internal,
            args,
            LibraryConstructorOptions {
                arg_names,
                applied_classifier: alias_target,
                expected,
                priority: ConstructorPriorityTier::All,
                selected,
            },
        ) {
            Ok(LibraryConstructorSelection::Selected) => {
                if let Some(receiver) = explicit_outer {
                    self.resolved_constructors
                        .get_mut(&call)
                        .expect("selected dependency constructor")
                        .bind_outer(receiver);
                }
                let instance = self.ctor_result_name_without_captures(
                    scope,
                    call,
                    internal,
                    expected,
                    alias_target,
                );
                Some(self.attach_captured_classifier_arguments(
                    scope,
                    call,
                    internal,
                    instance,
                    Some(receiver),
                ))
            }
            Ok(LibraryConstructorSelection::Rejected) => Some(Ty::Error),
            Ok(LibraryConstructorSelection::NoMatch) => {
                crate::trace_compiler!(
                    "resolve",
                    "bound inner constructor no match call={call:?} classifier={}",
                    internal.render(),
                );
                None
            }
            Err(error) => {
                self.report_library_constructor_failure(scope, call, args, error);
                Some(Ty::Error)
            }
        }
    }

    /// Materialize the constructor that member-level selection chose over the combined family.
    pub(super) fn commit_selected_inner_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        construction: BoundInnerConstruction<'_>,
        (internal, alias_target): (TypeName, Option<Ty>),
        selected: &crate::libraries::FunctionInfo,
    ) -> MemberSlotCall {
        let call = construction.call_args.call;
        match self.commit_bound_inner_constructor(
            scope,
            construction,
            internal,
            alias_target,
            Some(selected),
        ) {
            Some(Ty::Error) => MemberSlotCall::Rejected,
            Some(ret) => MemberSlotCall::Resolved(ret),
            None => {
                self.diags.error(
                    self.call_callee_name_span(call),
                    format!(
                        "selected constructor of '{}' does not accept the call's arguments",
                        internal.render()
                    ),
                );
                MemberSlotCall::Rejected
            }
        }
    }

    /// The inner classifier that joins `receiver`'s same-named member functions on its member
    /// level: a classifier the receiver's hierarchy declares, never an in-scope typealias.
    pub(super) fn member_level_inner_classifier(
        &self,
        receiver: Ty,
        name: &str,
    ) -> InheritedNestedClassifier {
        self.bound_inner_constructor_classifier(receiver, name)
    }

    fn bound_inner_constructor_classifier(
        &self,
        receiver: Ty,
        name: &str,
    ) -> InheritedNestedClassifier {
        let Some(root) = receiver.kotlin_class_internal() else {
            return InheritedNestedClassifier::NotFound;
        };
        let source = self.fed_source();
        let mut level = vec![root];
        let mut seen = std::collections::HashSet::new();
        while !level.is_empty() {
            let mut matches = std::collections::HashSet::new();
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
                        .is_some_and(|outer| {
                            self.receiver_is_assignable(receiver, Ty::obj_name(outer))
                        })
                }) {
                    matches.insert(classifier);
                }
                next.extend(
                    crate::symbol_resolver::direct_supertypes(&source, Ty::obj_name(owner))
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
}
