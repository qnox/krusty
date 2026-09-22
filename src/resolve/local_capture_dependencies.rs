//! Selected closure dependencies of body-local classifier members.
//!
//! Local classifier members are checked as independent Pass-2 units. If one selects an enclosing
//! local callable or another local classifier's constructor, its owner must carry the selected
//! declaration's closure ABI through its own constructor. This module derives that transitive edge
//! from resolved declaration identities after selection; it never substitutes syntax spelling for
//! applicability or constructor identity.

use super::*;

struct SelectedLocalClassifierCapture {
    capture: AnonymousObjectCapture,
    lexical_binding: Option<u32>,
}

struct SelectedLocalClassifierCaptures {
    declaration: DeclId,
    owner: TypeName,
    captures: Vec<SelectedLocalClassifierCapture>,
}

impl Checker<'_> {
    pub(super) fn extend_selected_local_dependency_captures(
        &self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        class: &ClassDecl,
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
    ) {
        debug_assert_eq!(captures.len(), capture_bindings.len());
        let expressions = self.local_class_dependency_expressions(class);
        self.extend_selected_callable_captures(scope, &expressions, captures, capture_bindings);
        self.extend_selected_constructor_captures(
            scope,
            declaration,
            &expressions,
            captures,
            capture_bindings,
        );
        self.extend_superclass_captures(scope, declaration, captures, capture_bindings);
        debug_assert_eq!(captures.len(), capture_bindings.len());
    }

    pub(super) fn local_capture_binding_identities(
        &self,
        scope: &CheckerScope<'_>,
        captures: &[AnonymousObjectCapture],
    ) -> Vec<Option<u32>> {
        captures
            .iter()
            .map(|capture| {
                (capture.source == AnonymousObjectCaptureSource::LexicalValue)
                    .then(|| {
                        self.lookup(scope, &capture.name)
                            .and_then(|binding| binding.lexical_capture_identity)
                    })
                    .flatten()
            })
            .collect()
    }

    pub(super) fn extend_anonymous_superclass_captures(
        &self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        captures: &mut Vec<AnonymousObjectCapture>,
    ) {
        let mut bindings = self.local_capture_binding_identities(scope, captures);
        self.extend_superclass_captures(scope, declaration, captures, &mut bindings);
    }

    fn lexical_binding_by_identity(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        identity: u32,
    ) -> Option<(Local, u32)> {
        let mut nearer = Vec::new();
        for rung in scope.ancestors() {
            let Some(binding) = rung
                .own_binding(name, Ns::Value)
                .and_then(|binding| binding.value())
            else {
                continue;
            };
            let Some(candidate) = binding.lexical_capture_identity else {
                continue;
            };
            if nearer.contains(&candidate) {
                continue;
            }
            if candidate == identity {
                return Some((binding, u32::try_from(nearer.len()).ok()?));
            }
            nearer.push(candidate);
        }
        None
    }

    fn merge_or_push_local_dependency_capture(
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
        candidate: AnonymousObjectCapture,
        binding: Option<u32>,
    ) {
        let existing = captures.iter().enumerate().find_map(|(index, capture)| {
            // The same semantic source is one capture, whether or not a dependency asked for it.
            // Lexical values require their resolved binding identity; receiver/storage captures
            // carry their exact coordinate in `source`. Spelling is never part of this join.
            let same_source = match (capture_bindings[index], binding) {
                (Some(existing), Some(candidate)) => existing == candidate,
                (None, None)
                    if capture.source != AnonymousObjectCaptureSource::LexicalValue
                        && candidate.source != AnonymousObjectCaptureSource::LexicalValue =>
                {
                    capture.source == candidate.source
                }
                _ => false,
            };
            let same_semantic_source = match candidate.capture_dependency {
                Some(dependency) => capture.capture_dependency == Some(dependency) || same_source,
                None => capture.capture_dependency.is_none() && same_source,
            };
            same_semantic_source.then_some(index)
        });
        if let Some(existing) = existing {
            captures[existing].shared_cell |= candidate.shared_cell;
            captures[existing].capture_dependency = captures[existing]
                .capture_dependency
                .or(candidate.capture_dependency);
            return;
        }
        captures.push(candidate);
        capture_bindings.push(binding);
    }

    fn local_class_dependency_expressions(&self, class: &ClassDecl) -> Vec<ExprId> {
        fn visit(file: &File, expression: ExprId, expressions: &mut Vec<ExprId>) {
            expressions.push(expression);
            let mut children = Vec::new();
            let mut statements = Vec::new();
            file.any_child_expr(
                expression,
                &mut |child| {
                    children.push(child);
                    false
                },
                &mut |statement| {
                    statements.push(statement);
                    false
                },
            );
            for child in children {
                visit(file, child, expressions);
            }
            for statement in statements {
                file.any_child_stmt(statement, &mut |child| {
                    visit(file, child, expressions);
                    false
                });
            }
        }

        let mut expressions = Vec::new();
        for root in local_class_capture_expressions(class) {
            visit(self.file, root, &mut expressions);
        }
        expressions
    }

    fn extend_selected_callable_captures(
        &self,
        scope: &CheckerScope<'_>,
        expressions: &[ExprId],
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
    ) {
        let mut selected = expressions
            .iter()
            .filter_map(|expression| {
                self.resolved_calls
                    .get(expression)
                    .and_then(|call| match call {
                        ResolvedCall::LocalFunction(call) => Some(call.stmt_id),
                        _ => None,
                    })
                    .or_else(|| match self.expr_lowers.get(expression) {
                        Some(ExprLowering::LocalFunction { stmt_id, .. }) => Some(*stmt_id),
                        _ => None,
                    })
            })
            .collect::<Vec<_>>();
        selected.sort_by_key(|statement| statement.0);
        selected.dedup();
        for statement in selected {
            let Some(StmtLowering::LocalFunction(function)) = self.stmt_lowers.get(&statement)
            else {
                continue;
            };
            let required_bindings = self.local_function_capture_bindings.get(&statement);
            for (ordinal, required) in function.captures.iter().enumerate() {
                let required_identity = required_bindings
                    .and_then(|bindings| bindings.get(ordinal))
                    .copied()
                    .flatten();
                let selected = required_identity.and_then(|identity| {
                    self.lexical_binding_by_identity(scope, &required.name, identity)
                });
                let Some((binding, lexical_shadow_depth)) = selected else {
                    continue;
                };
                Self::merge_or_push_local_dependency_capture(
                    captures,
                    capture_bindings,
                    AnonymousObjectCapture {
                        name: required.name.clone(),
                        ty: binding.ty,
                        shared_cell: required.shared_cell,
                        storage_ty: binding.delegate_storage_ty,
                        source: AnonymousObjectCaptureSource::LexicalValue,
                        receiver_label: None,
                        lexical_shadow_depth,
                        capture_dependency: None,
                    },
                    required_identity,
                );
            }
        }
    }

    /// The captures a local class needs because its SUPERCLASS is a local class that has some.
    ///
    /// A supertype constructor call is not an expression: `class Derived : Local(true)` records the
    /// base classifier and its ARGUMENTS, and nothing in between that a resolved-constructor lookup
    /// could find. So the edge is read from the resolved supertype instead — the same declaration
    /// identity, arrived at from the header rather than from a call — and the superclass's own
    /// discovered captures become the subclass's, to be passed on ahead of the written arguments.
    ///
    /// Without this the subclass carries nothing and its constructor calls the base with the
    /// written arguments alone, one value short per capture: kotlinc compiles these, and both of
    /// krusty's backends were rejecting them — the JVM's with a `VerifyError` putting `this` where
    /// the capture belongs.
    fn extend_superclass_captures(
        &self,
        scope: &CheckerScope<'_>,
        current_declaration: DeclId,
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
    ) {
        let Decl::Class(class) = self.file.decl(current_declaration) else {
            return;
        };
        let Some(owner) = self.active_classifier_internal(current_declaration, class) else {
            return;
        };
        let Some(superclass) = self
            .resolved_body_local_supertypes
            .get(&owner)
            .and_then(|supertypes| supertypes.first())
            .and_then(|supertype| supertype.kotlin_class_internal())
        else {
            return;
        };
        let Some(selected) = self.local_classifier_captures_of(superclass) else {
            return;
        };
        self.extend_required_captures(
            scope,
            current_declaration,
            selected,
            captures,
            capture_bindings,
        );
    }

    fn extend_selected_constructor_captures(
        &self,
        scope: &CheckerScope<'_>,
        current_declaration: DeclId,
        expressions: &[ExprId],
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
    ) {
        let required = expressions
            .iter()
            .filter_map(|expression| self.resolved_constructors.get(expression))
            .filter_map(|constructor| self.selected_local_classifier_captures(constructor))
            .flat_map(|selected| {
                let SelectedLocalClassifierCaptures {
                    declaration,
                    owner,
                    captures,
                } = selected;
                captures
                    .into_iter()
                    .enumerate()
                    .map(move |(field, selected)| {
                        (
                            declaration,
                            owner,
                            field,
                            selected.capture,
                            selected.lexical_binding,
                        )
                    })
            })
            .collect::<Vec<_>>();
        self.extend_required_captures(
            scope,
            current_declaration,
            required,
            captures,
            capture_bindings,
        );
    }

    /// Carry each REQUIRED capture of a selected local declaration into the current one, mapping it
    /// onto a source this scope can supply. A capture with no such source here is skipped: the
    /// declaration that needs it is reached from somewhere that has it, or not at all.
    fn extend_required_captures(
        &self,
        scope: &CheckerScope<'_>,
        current_declaration: DeclId,
        required: Vec<(DeclId, TypeName, usize, AnonymousObjectCapture, Option<u32>)>,
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
    ) {
        let receivers = self.implicit_receivers(scope);
        for (declaration, owner, field, mut required, required_identity) in required {
            if declaration == current_declaration {
                continue;
            }
            let mut lexical_shadow_depth = required.lexical_shadow_depth;
            let source = match required.source {
                AnonymousObjectCaptureSource::LexicalValue => {
                    let selected = required_identity.and_then(|identity| {
                        self.lexical_binding_by_identity(scope, &required.name, identity)
                    });
                    let Some((binding, shadow_depth)) = selected else {
                        continue;
                    };
                    lexical_shadow_depth = shadow_depth;
                    match binding.origin {
                        ReceiverFnValueOrigin::Local => AnonymousObjectCaptureSource::LexicalValue,
                        ReceiverFnValueOrigin::ClassStorage(field) => {
                            AnonymousObjectCaptureSource::ClassStorage { field }
                        }
                        ReceiverFnValueOrigin::DispatchProperty { .. }
                        | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. }
                        | ReceiverFnValueOrigin::TopLevelProperty => continue,
                    }
                }
                AnonymousObjectCaptureSource::ClassStorage { .. } => required.source,
                AnonymousObjectCaptureSource::EnclosingInstance { current, depth }
                | AnonymousObjectCaptureSource::ImplicitReceiver { current, depth } => {
                    if !receivers.iter().any(|receiver| {
                        receiver.current == current
                            && receiver.receiver_depth == depth as usize
                            && receiver.ty == required.ty
                    }) {
                        continue;
                    }
                    required.source
                }
            };
            required.source = source;
            required.lexical_shadow_depth = lexical_shadow_depth;
            required.capture_dependency = required.capture_dependency.or_else(|| {
                u32::try_from(field)
                    .ok()
                    .map(|field| crate::fir::ClassCaptureIdentity { owner, field })
            });
            Self::merge_or_push_local_dependency_capture(
                captures,
                capture_bindings,
                required,
                required_identity,
            );
        }
    }

    /// The discovered captures of the local classifier named `owner`, in the shape
    /// [`Self::extend_required_captures`] consumes.
    fn local_classifier_captures_of(
        &self,
        owner: TypeName,
    ) -> Option<Vec<(DeclId, TypeName, usize, AnonymousObjectCapture, Option<u32>)>> {
        let declaration =
            self.discovered_local_class_captures
                .keys()
                .copied()
                .find(|declaration| {
                    matches!(
                        self.file.decl(*declaration),
                        Decl::Class(class)
                            if self.active_classifier_internal(*declaration, class) == Some(owner)
                    )
                })?;
        let captures = self.discovered_local_class_captures.get(&declaration)?;
        let bindings = self
            .discovered_local_class_capture_bindings
            .get(&declaration)?;
        (captures.len() == bindings.len()).then(|| {
            captures
                .iter()
                .cloned()
                .zip(bindings.iter().copied())
                .enumerate()
                .map(|(field, (capture, binding))| (declaration, owner, field, capture, binding))
                .collect()
        })
    }

    fn selected_local_classifier_captures(
        &self,
        constructor: &ResolvedConstructor,
    ) -> Option<SelectedLocalClassifierCaptures> {
        let stable_constructor = match constructor {
            ResolvedConstructor::Source {
                stable_declaration, ..
            } => *stable_declaration,
            ResolvedConstructor::Plain { member, .. }
            | ResolvedConstructor::PlainSlots { member, .. } => member.stable_declaration,
            ResolvedConstructor::Synthetic { ctor, .. } => ctor.declaration.stable_declaration,
        };
        let classifier = stable_constructor
            .and_then(|declaration| {
                self.active_declarations
                    .and_then(|active| active.constructor(self.file, declaration))
                    .map(|(classifier, _, _)| classifier)
            })
            .or_else(|| {
                let owner = constructor.owner();
                self.discovered_local_class_captures
                    .keys()
                    .copied()
                    .find(|declaration| {
                        matches!(
                            self.file.decl(*declaration),
                            Decl::Class(class)
                                if self.active_classifier_internal(*declaration, class) == Some(owner)
                        )
                    })
            })?;
        let captures = self.discovered_local_class_captures.get(&classifier)?;
        let bindings = self
            .discovered_local_class_capture_bindings
            .get(&classifier)?;
        (captures.len() == bindings.len()).then(|| SelectedLocalClassifierCaptures {
            declaration: classifier,
            owner: constructor.owner(),
            captures: captures
                .iter()
                .cloned()
                .zip(bindings.iter().copied())
                .map(
                    |(capture, lexical_binding)| SelectedLocalClassifierCapture {
                        capture,
                        lexical_binding,
                    },
                )
                .collect(),
        })
    }
}
