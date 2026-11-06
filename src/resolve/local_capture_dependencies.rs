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

struct RequiredLocalClassifierCapture {
    declaration: DeclId,
    owner: TypeName,
    capture: AnonymousObjectCapture,
    lexical_binding: Option<u32>,
}

/// Only a live scoped receiver proves which class boundary separates it from this call.
/// Reconstructed declaration-role receivers keep their existing materialization contract.
fn live_receiver_crosses_class(scope: &CheckerScope<'_>, identity: (usize, usize)) -> Option<bool> {
    if !scope
        .implicit_receivers_with_declarations()
        .iter()
        .any(|(_, _, live, _)| *live == identity)
    {
        return None;
    }
    let mut crossed = false;
    for rung in scope.ancestors() {
        if rung as *const _ as usize == identity.0 {
            return Some(crossed);
        }
        crossed |= matches!(rung.kind(), scope::ScopeKind::Class { .. });
    }
    None
}

impl Checker<'_> {
    pub(super) fn extend_selected_local_dependency_captures(
        &mut self,
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
        captures: &[AnonymousObjectCapture],
    ) -> Vec<Option<u32>> {
        captures
            .iter()
            .map(|capture| capture.lexical_binding)
            .collect()
    }

    pub(super) fn extend_anonymous_superclass_captures(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        captures: &mut Vec<AnonymousObjectCapture>,
    ) {
        let mut bindings = Self::local_capture_binding_identities(captures);
        self.extend_superclass_captures(scope, declaration, captures, &mut bindings);
    }

    pub(super) fn lexical_binding_by_identity(
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
            // Lexical values require their resolved binding identity; receivers retain their
            // declaration role, and storage carries its logical source key. Coordinates and
            // spelling are never identities in this join.
            let same_source = match (capture_bindings[index], binding) {
                (Some(existing), Some(candidate)) => existing == candidate,
                (None, None)
                    if capture.source != AnonymousObjectCaptureSource::LexicalValue
                        && candidate.source != AnonymousObjectCaptureSource::LexicalValue =>
                {
                    match (capture.source, candidate.source) {
                        (
                            AnonymousObjectCaptureSource::ClassStorage(left),
                            AnonymousObjectCaptureSource::ClassStorage(right),
                        ) => left.identity == right.identity,
                        (
                            AnonymousObjectCaptureSource::ImplicitReceiver { .. },
                            AnonymousObjectCaptureSource::ImplicitReceiver { .. },
                        )
                        | (
                            AnonymousObjectCaptureSource::EnclosingInstance { .. },
                            AnonymousObjectCaptureSource::EnclosingInstance { .. },
                        ) => capture.same_direct_receiver(&candidate),
                        (left, right) => left == right,
                    }
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
            captures[existing].retain_constructor_projections(&candidate.constructor_projections);
            // Demand aggregation cannot retag storage already selected by the class's own body.
            // Each constructor edge retains its required family and projects its actual operand.
            if captures[existing].receiver_role.is_none() && candidate.receiver_role.is_some() {
                captures[existing].receiver_capture = candidate.receiver_capture;
                captures[existing].receiver_role = candidate.receiver_role;
            } else if candidate.receiver_role.is_some() {
                assert_eq!(
                    captures[existing].receiver_capture,
                    candidate.receiver_capture
                );
                assert_eq!(captures[existing].receiver_role, candidate.receiver_role);
            }
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
            for required in &function.captures {
                let required_identity = required.lexical_binding;
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
                        receiver: None,
                        semantic_receiver: None,
                        lexical_shadow_depth,
                        lexical_binding: required_identity,
                        capture_dependency: None,
                        constructor_projections: Vec::new(),
                        receiver_capture: None,
                        receiver_role: None,
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
        &mut self,
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
            .applied_declared_supertype(owner)
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
        &mut self,
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
                    .map(move |selected| RequiredLocalClassifierCapture {
                        capture: selected.capture,
                        declaration,
                        owner,
                        lexical_binding: selected.lexical_binding,
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
        &mut self,
        scope: &CheckerScope<'_>,
        current_declaration: DeclId,
        required: Vec<RequiredLocalClassifierCapture>,
        captures: &mut Vec<AnonymousObjectCapture>,
        capture_bindings: &mut Vec<Option<u32>>,
    ) {
        let receivers = self.implicit_receivers(scope);
        let current_owner = self.body_class_storage_owner(current_declaration);
        for RequiredLocalClassifierCapture {
            declaration,
            owner,
            capture: mut required,
            lexical_binding: required_identity,
        } in required
        {
            if declaration == current_declaration {
                continue;
            }
            let Some(wanted) = required.logical_identity(owner) else {
                self.diags.error(
                    crate::diag::Span::new(0, 0),
                    "required capture has no selected logical identity",
                );
                return;
            };
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
                    if !required.constructor_projections.contains(&wanted) {
                        required.constructor_projections.push(wanted);
                    }
                    match binding.origin {
                        ReceiverFnValueOrigin::Local => AnonymousObjectCaptureSource::LexicalValue,
                        ReceiverFnValueOrigin::ClassStorage(storage) => {
                            if current_owner == Some(storage.owner) {
                                if let Some(existing) = {
                                    let mut selected =
                                        captures.iter().enumerate().filter(|(_, capture)| {
                                            capture.represents_identity(
                                                storage.owner,
                                                storage.identity,
                                            )
                                        });
                                    let index = selected.next().map(|(index, _)| index);
                                    let unique = selected.next().is_none();
                                    index
                                        .filter(|_| unique)
                                        .and_then(|index| captures.get_mut(index))
                                } {
                                    existing.shared_cell |= required.shared_cell;
                                    existing.retain_constructor_projections(
                                        &required.constructor_projections,
                                    );
                                    continue;
                                }
                            }
                            AnonymousObjectCaptureSource::ClassStorage(storage)
                        }
                        ReceiverFnValueOrigin::DispatchProperty { .. }
                        | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. }
                        | ReceiverFnValueOrigin::TopLevelProperty => continue,
                    }
                }
                AnonymousObjectCaptureSource::ClassStorage(storage) => {
                    // The nested constructor already reads this exact field from the current local
                    // classifier. Carrying it through the current classifier's own constructor
                    // would duplicate the field and reinterpret its ordinal under another owner.
                    if current_owner == Some(storage.owner) {
                        if let Some(existing) = {
                            let mut selected =
                                captures.iter().enumerate().filter(|(_, capture)| {
                                    capture.represents_identity(storage.owner, storage.identity)
                                });
                            let index = selected.next().map(|(index, _)| index);
                            let unique = selected.next().is_none();
                            index
                                .filter(|_| unique)
                                .and_then(|index| captures.get_mut(index))
                        } {
                            existing.shared_cell |= required.shared_cell;
                            existing
                                .retain_constructor_projections(&required.constructor_projections);
                            continue;
                        }
                    }
                    required.source
                }
                AnonymousObjectCaptureSource::EnumEntryPropertyStorage { .. } => required.source,
                AnonymousObjectCaptureSource::EnclosingInstance { .. }
                | AnonymousObjectCaptureSource::ImplicitReceiver { .. } => {
                    let Some(role) = required.receiver_role else {
                        self.diags.error(
                            crate::diag::Span::new(0, 0),
                            "required receiver capture has no declaration role",
                        );
                        return;
                    };
                    let mut selected = receivers
                        .iter()
                        .filter(|receiver| receiver.receiver_role == Some(role));
                    let Some(receiver) = selected.next() else {
                        // A receiver introduced inside the selected call's lambda is supplied there,
                        // not captured into the classifier surrounding that lambda.
                        continue;
                    };
                    if selected.next().is_some() {
                        self.diags.error(
                            crate::diag::Span::new(0, 0),
                            "required receiver capture has ambiguous declaration roles",
                        );
                        return;
                    }
                    if receiver.ty != required.ty {
                        continue;
                    }
                    let depth = u32::try_from(receiver.receiver_depth)
                        .expect("too many implicit receiver rungs");
                    let source = if receiver.class_receiver {
                        AnonymousObjectCaptureSource::EnclosingInstance {
                            current: receiver.current,
                            depth,
                        }
                    } else {
                        AnonymousObjectCaptureSource::ImplicitReceiver {
                            current: receiver.current,
                            depth,
                        }
                    };
                    required.semantic_receiver = Some(if receiver.class_receiver {
                        AnonymousObjectReceiverSource::EnclosingInstance {
                            current: receiver.current,
                            depth,
                        }
                    } else {
                        AnonymousObjectReceiverSource::ImplicitReceiver {
                            current: receiver.current,
                            depth,
                        }
                    });
                    if !required.constructor_projections.contains(&wanted) {
                        required.constructor_projections.push(wanted);
                    }
                    source
                }
            };
            required.lexical_shadow_depth = lexical_shadow_depth;
            let Some(required) = required.forwarded_from(owner, source) else {
                self.diags.error(
                    crate::diag::Span::new(0, 0),
                    "required capture has no selected logical identity",
                );
                return;
            };
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
    /// The local superclass of a local class, when it is one whose captures were discovered.
    pub(super) fn local_superclass_declaration(
        &self,
        declaration: DeclId,
        class: &ClassDecl,
    ) -> Option<DeclId> {
        let owner = self.active_classifier_internal(declaration, class)?;
        let superclass = self
            .resolved_body_local_supertypes
            .get(&owner)?
            .first()?
            .kotlin_class_internal()?;
        self.discovered_local_classifier(superclass)
    }

    fn discovered_local_classifier(&self, owner: TypeName) -> Option<DeclId> {
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
    }

    /// Commit sources while the selected declaration and its lexical binding tower are alive.
    pub(super) fn selected_constructor_capture_plan(
        &self,
        scope: &CheckerScope<'_>,
        owner: TypeName,
    ) -> Option<super::constructor_commitment::ConstructorCapturePlan> {
        use super::constructor_commitment::{
            ConstructorCapturePlan as Plan, ConstructorCaptureProjection,
            ConstructorCaptureSource as Source,
        };
        let declaration = self.file.local_class_decls.values().copied().find(|&declaration| {
            matches!(self.file.decl(declaration), Decl::Class(class) if self.active_classifier_internal(declaration, class) == Some(owner))
        })?;
        if self
            .implicit_receivers(scope)
            .into_iter()
            .find(|receiver| receiver.class_receiver)
            .and_then(|receiver| receiver.receiver_role)
            == Some(ReceiverDeclarationRole::EnclosingClass(owner))
        {
            return Some(Plan::OwnStorage(owner));
        }
        let captures = match self.discovered_local_class_captures.get(&declaration) {
            Some(captures) => captures.clone(),
            None if self.finalized_local_class_captures.contains(&declaration) => Vec::new(),
            None => return None,
        };
        let mut projected = Vec::with_capacity(captures.len());
        for capture in captures {
            let required = capture.logical_identity(owner)?;
            let source = if let Some(identity) = capture.lexical_binding {
                let (binding, _) =
                    self.lexical_binding_by_identity(scope, &capture.name, identity)?;
                match binding.origin {
                    ReceiverFnValueOrigin::ClassStorage(storage) => Source::ClassStorage(storage),
                    ReceiverFnValueOrigin::EnumEntryPropertyStorage { owner, field } => {
                        Source::EnumEntryProperty {
                            owner,
                            ordinal: field,
                        }
                    }
                    ReceiverFnValueOrigin::Local => {
                        let mut crossed_class = false;
                        for rung in scope.ancestors() {
                            if rung
                                .own_binding(&capture.name, Ns::Value)
                                .and_then(|binding| binding.value())
                                .is_some_and(|binding| {
                                    binding.lexical_capture_identity == Some(identity)
                                })
                            {
                                break;
                            }
                            crossed_class |= matches!(rung.kind(), scope::ScopeKind::Class { .. });
                        }
                        if crossed_class {
                            projected.push(ConstructorCaptureProjection {
                                required,
                                source: Source::Captured,
                            });
                            continue;
                        }
                        let delegated = binding.delegate_storage_ty.is_some();
                        let mut seen = HashSet::default();
                        let shadow_depth = scope
                            .ancestors()
                            .filter_map(|rung| {
                                rung.own_binding(&capture.name, Ns::Value)
                                    .and_then(|binding| binding.value())
                            })
                            .filter(|binding| {
                                matches!(binding.origin, ReceiverFnValueOrigin::Local)
                                    && binding.delegate_storage_ty.is_some() == delegated
                            })
                            .filter(|binding| {
                                binding
                                    .lexical_capture_identity
                                    .is_some_and(|identity| seen.insert(identity))
                            })
                            .position(|binding| binding.lexical_capture_identity == Some(identity))
                            .and_then(|depth| u32::try_from(depth).ok());
                        let shadow_depth = shadow_depth?;
                        Source::Lexical {
                            name: capture.name.clone(),
                            shadow_depth,
                            delegated,
                        }
                    }
                    _ => return None,
                }
            } else {
                match capture.source {
                    AnonymousObjectCaptureSource::ClassStorage(storage) => {
                        Source::ClassStorage(storage)
                    }
                    AnonymousObjectCaptureSource::EnumEntryPropertyStorage { owner, field } => {
                        Source::EnumEntryProperty {
                            owner,
                            ordinal: field,
                        }
                    }
                    AnonymousObjectCaptureSource::EnclosingInstance { .. }
                    | AnonymousObjectCaptureSource::ImplicitReceiver { .. } => {
                        let role = capture.receiver_role?;
                        let mut receivers = self
                            .implicit_receivers(scope)
                            .into_iter()
                            .filter(|receiver| receiver.receiver_role == Some(role));
                        let selected = receivers.next()?;
                        if receivers.next().is_some() {
                            return None;
                        }
                        if live_receiver_crosses_class(scope, selected.identity) == Some(true) {
                            // The selected constructor demand carries this same receiver identity
                            // into the intervening classifier. FIR reads its complete binding,
                            // including the constructor/lambda lifetime, after capture publication.
                            Source::Captured
                        } else {
                            Source::Receiver(self.implicit_receiver_selection(selected))
                        }
                    }
                    AnonymousObjectCaptureSource::LexicalValue => return None,
                }
            };
            projected.push(ConstructorCaptureProjection { required, source });
        }
        Some(Plan::Selected(projected.into_boxed_slice()))
    }

    fn local_classifier_captures_of(
        &self,
        owner: TypeName,
    ) -> Option<Vec<RequiredLocalClassifierCapture>> {
        let declaration = self.discovered_local_classifier(owner)?;
        let captures = self.discovered_local_class_captures.get(&declaration)?;
        let bindings = self
            .discovered_local_class_capture_bindings
            .get(&declaration)?;
        (captures.len() == bindings.len()).then(|| {
            captures
                .iter()
                .cloned()
                .zip(bindings.iter().copied())
                .map(
                    |(capture, lexical_binding)| RequiredLocalClassifierCapture {
                        declaration,
                        owner,
                        capture,
                        lexical_binding,
                    },
                )
                .collect()
        })
    }

    fn selected_local_classifier_captures(
        &self,
        constructor: &ResolvedConstructor,
    ) -> Option<SelectedLocalClassifierCaptures> {
        let classifier = self.constructed_local_classifier(constructor)?;
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

    /// The local classifier a resolved constructor builds.
    pub(super) fn constructed_local_classifier(
        &self,
        constructor: &ResolvedConstructor,
    ) -> Option<DeclId> {
        let stable_constructor = match constructor {
            ResolvedConstructor::Source {
                stable_declaration, ..
            } => *stable_declaration,
            ResolvedConstructor::Plain { member, .. }
            | ResolvedConstructor::PlainSlots { member, .. } => member.stable_declaration,
            ResolvedConstructor::Synthetic { ctor, .. } => ctor.declaration.stable_declaration,
        };
        stable_constructor
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
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(owner: TypeName) -> AnonymousObjectCapture {
        AnonymousObjectCapture {
            name: "value".into(),
            ty: Ty::String,
            shared_cell: false,
            storage_ty: None,
            source: AnonymousObjectCaptureSource::ClassStorage(ClassStorageSource {
                owner,
                identity: crate::fir::ClassCaptureIdentity::Value { owner, binding: 7 },
            }),
            receiver_label: None,
            receiver: None,
            semantic_receiver: None,
            lexical_shadow_depth: 0,
            lexical_binding: Some(7),
            capture_dependency: None,
            constructor_projections: Vec::new(),
            receiver_capture: None,
            receiver_role: None,
        }
    }

    #[test]
    fn resolved_constructor_demands_share_storage_without_retagging_its_body_identity() {
        let a = type_name("PhysicalCaptureA");
        let mut captures = vec![capture(a)];
        let mut bindings = vec![Some(7)];
        let canonical = captures[0].logical_identity(a);
        let mut wanted = Vec::new();
        for owner in [type_name("ConstructorB"), type_name("ConstructorC")] {
            let identity = crate::fir::ClassCaptureIdentity::Value { owner, binding: 7 };
            let mut demand = capture(a);
            demand.capture_dependency = Some(identity);
            demand.constructor_projections.push(identity);
            // This binding was selected in the same bounded scope, before this aggregation step.
            Checker::merge_or_push_local_dependency_capture(
                &mut captures,
                &mut bindings,
                demand,
                Some(7),
            );
            wanted.push(identity);
        }
        let [actual] = captures.as_slice() else {
            panic!("one physical capture")
        };
        assert_eq!(bindings, [Some(7)]);
        assert_eq!(actual.logical_identity(a), canonical);
        assert_eq!(actual.constructor_projections, wanted);
        assert_eq!(actual.source, capture(a).source);
        let (revisited, fields) =
            super::super::capture_field_order::reconcile(Some(&captures), vec![capture(a)], false);
        assert_eq!(fields, [Some(0)]);
        assert_eq!(revisited[0].logical_identity(a), canonical);
        assert_eq!(revisited[0].constructor_projections, wanted);
    }

    #[test]
    fn equal_raw_binding_numbers_from_other_storage_families_do_not_prove_a_projection() {
        let a = type_name("PhysicalCaptureA");
        let other = type_name("UnrelatedCaptureOwner");
        let mut captures = vec![capture(a)];
        let mut bindings = vec![None];
        Checker::merge_or_push_local_dependency_capture(
            &mut captures,
            &mut bindings,
            capture(other),
            None,
        );
        assert_eq!(captures.len(), 2);
        assert_ne!(
            captures[0].logical_identity(a),
            captures[1].logical_identity(other)
        );
        assert!(captures
            .iter()
            .all(|capture| capture.constructor_projections.is_empty()));
    }

    fn receiver(identity: u32, declaration: u32, depth: u32) -> AnonymousObjectCapture {
        let mut capture = capture(type_name("ReceiverCaptureHost"));
        capture.lexical_binding = None;
        capture.source = AnonymousObjectCaptureSource::ImplicitReceiver {
            current: depth == 0,
            depth,
        };
        capture.semantic_receiver = Some(AnonymousObjectReceiverSource::ImplicitReceiver {
            current: depth == 0,
            depth,
        });
        capture.receiver_capture = Some(identity);
        capture.receiver_role = Some(ReceiverDeclarationRole::Extension(crate::diag::Span {
            lo: declaration,
            hi: declaration + 1,
        }));
        capture
    }

    #[test]
    fn constructor_only_receiver_demand_survives_without_retagging_its_physical_capture() {
        let original = receiver(0, 10, 0);
        let mut captures = vec![original.clone()];
        let mut bindings = vec![None];
        let wanted = crate::fir::ClassCaptureIdentity::Receiver(0);
        let mut required = original.clone();
        required.capture_dependency = Some(wanted);
        required.constructor_projections.push(wanted);
        Checker::merge_or_push_local_dependency_capture(
            &mut captures,
            &mut bindings,
            required,
            None,
        );
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].capture_dependency, None);
        assert_eq!(captures[0].constructor_projections, [wanted]);
        super::super::anonymous_object_capture::drop_unused_implicit_receivers(
            &mut captures,
            &mut bindings,
            &[],
        );
        assert_eq!(bindings, [None]);
        assert_eq!(captures[0].source, original.source);
        assert_eq!(captures[0].receiver_capture, original.receiver_capture);
        let mut shifted = receiver(0, 10, 2);
        shifted.constructor_projections = captures[0].constructor_projections.clone();
        let (revisited, fields) = super::super::capture_field_order::reconcile(
            Some(&captures),
            vec![shifted.clone()],
            false,
        );
        assert_eq!(fields, [Some(0)]);
        assert_eq!(revisited, [shifted]);

        let mut unused = vec![original];
        let mut unused_bindings = vec![None];
        super::super::anonymous_object_capture::drop_unused_implicit_receivers(
            &mut unused,
            &mut unused_bindings,
            &[],
        );
        assert!(unused.is_empty());
        assert!(unused_bindings.is_empty());
    }

    #[test]
    fn equal_receiver_coordinates_never_merge_distinct_declaration_roles() {
        let mut captures = vec![receiver(0, 10, 0)];
        let mut bindings = vec![None];
        let mut required = receiver(1, 20, 0);
        required.capture_dependency = Some(crate::fir::ClassCaptureIdentity::Receiver(1));
        Checker::merge_or_push_local_dependency_capture(
            &mut captures,
            &mut bindings,
            required.clone(),
            None,
        );
        assert_eq!(captures, [receiver(0, 10, 0), required]);
        assert_eq!(bindings, [None, None]);
    }

    #[test]
    fn enclosing_instance_roles_remain_distinct_without_closure_ids() {
        let mut enclosing = receiver(0, 10, 0);
        enclosing.receiver_capture = None;
        enclosing.receiver_role = Some(ReceiverDeclarationRole::EnclosingClass(type_name("Outer")));
        enclosing.source = AnonymousObjectCaptureSource::EnclosingInstance {
            current: true,
            depth: 0,
        };
        let mut shifted = enclosing.clone();
        shifted.source = AnonymousObjectCaptureSource::EnclosingInstance {
            current: false,
            depth: 2,
        };
        let mut other = enclosing.clone();
        other.receiver_role = Some(ReceiverDeclarationRole::EnclosingClass(type_name("Other")));
        assert!(shifted.same_direct_receiver(&enclosing));
        assert!(!other.same_direct_receiver(&enclosing));
        let (revisited, fields) = super::super::capture_field_order::reconcile(
            Some(&[enclosing]),
            vec![other.clone(), shifted.clone()],
            false,
        );
        assert_eq!(fields, [Some(1)]);
        assert_eq!(revisited, [other, shifted]);
    }
}
