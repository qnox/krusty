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
                let selected = target
                    .and_then(Ty::kotlin_class_internal)
                    .filter(|classifier| {
                        self.fed_source()
                            .classifier(*classifier)
                            .and_then(|shape| shape.outer_instance)
                            .is_some_and(|outer| {
                                self.receiver_is_assignable(receiver, Ty::obj_name(outer))
                            })
                    });
                match selected {
                    Some(internal) => BoundInnerClassifier::Found {
                        internal,
                        alias_target: target,
                    },
                    None => BoundInnerClassifier::NotFound,
                }
            }
        }
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
        // The receiver supplies the captured outer instance, so its application of the outer
        // classifier fixes the enclosing type parameters the constructor's declared shapes
        // reference. The preselected member-level candidate was already specialized by selection;
        // only a constructor-family selection here still needs the receiver's bindings.
        let enclosing_bindings = if selected.is_some() {
            None
        } else {
            let source = self.fed_source();
            self.resolved_type_name(internal).and_then(|classifier| {
                crate::symbol_resolver::outer_instance_bindings(&source, receiver, &classifier)
                    .map(|(bindings, _)| bindings)
            })
        };
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
                enclosing_bindings,
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
