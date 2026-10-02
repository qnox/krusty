//! What a local class or anonymous object captures at its construction site: the checker's
//! contract with checked FIR, which turns each capture into a constructor parameter and field.

use super::capture_storage::{
    anonymous_body_expressions, anonymous_descendant_uses_name, anonymous_descendant_writes_name,
    anonymous_descendants, enclosing_value_visible_beside_member,
};
use super::scope::ContextReceiverKind;
use super::{
    capture_field_order, AnonymousLexicalClassScope, Checker, CheckerScope, ExprLowering,
    ResolvedCall, StmtLowering,
};
use crate::ast::{DeclId, ExprId, File, StmtId};
use crate::diag::Span;
use crate::fir::FirCapturedReceiver;
use crate::types::{CapturedContextKind, Ty};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnonymousObjectCapture {
    pub name: String,
    pub ty: Ty,
    /// The enclosing mutable local is represented by one shared cell rather than copied by value.
    /// This is a semantic capture decision; the backend chooses the cell representation.
    pub shared_cell: bool,
    /// Physical value captured by the generated class. A delegated property's semantic type remains
    /// `ty`, while its immutable delegate object is the constructor/field payload.
    pub storage_ty: Option<Ty>,
    /// Semantic source of the captured value. Keep this separate from `name`: `this$0` is one JVM
    /// field spelling, not a reliable front-end discriminator. A backend or future target may choose
    /// a different physical name while the enclosing-instance meaning remains unchanged.
    pub source: AnonymousObjectCaptureSource,
    /// Source-level receiver label required while a retained inline/local classifier body is
    /// checked in isolation. This is present only for an implicit receiver capture; checked FIR
    /// consumes the semantic receiver coordinate and does not retain the spelling.
    pub receiver_label: Option<Box<str>>,
    /// What a captured receiver was in source, which names the capture's field; `None` for a
    /// captured value.
    pub receiver: Option<FirCapturedReceiver>,
    /// Receiver-tower coordinate this capture represented before any enclosing anonymous field
    /// became its physical source. A forwarded super argument is evaluated outside that enclosing
    /// constructor and therefore rematerializes this semantic receiver rather than reading the
    /// field. Keep the complete typed coordinate: depth alone cannot distinguish a class receiver
    /// from an extension/context receiver, and `current` is independent of its numeric depth.
    pub semantic_receiver: Option<AnonymousObjectReceiverSource>,
    /// Number of distinct same-named lexical bindings nearer than the selected source at this
    /// construction site. This is a bounded-checker coordinate, not a source location; checked FIR
    /// consumes it while the active lexical scopes still exist.
    pub(crate) lexical_shadow_depth: u32,
    /// Semantic closure field forwarded by this capture. Direct captures leave this absent and
    /// establish their own identity when checked; transitive captures preserve the upstream field.
    pub(crate) capture_dependency: Option<crate::fir::ClassCaptureIdentity>,
    /// Stable identity of the implicit-receiver rung this capture holds. Absent for lexical values
    /// and for an enclosing class instance. Two classifiers that capture the same rung share it;
    /// the callable or lambda label is not this identity.
    pub(crate) receiver_capture: Option<u32>,
}

impl AnonymousObjectCapture {
    pub fn stored_ty(&self) -> Ty {
        self.storage_ty.unwrap_or(self.ty)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnonymousObjectCaptureSource {
    LexicalValue,
    /// A field of the construction site's current classifier, selected by the checker. The capture
    /// constructor reads this exact ordinal; neither FIR construction nor lowering looks the name up.
    ClassStorage {
        field: u32,
    },
    /// Exact class-receiver rung selected at the construction site.
    EnclosingInstance {
        current: bool,
        depth: u32,
    },
    /// A receiver introduced by an enclosing extension/receiver-function/context rung. The
    /// coordinate is relative to the construction body's checked receiver tower; it is semantic
    /// identity and must not be reconstructed from the capture field spelling.
    ImplicitReceiver {
        current: bool,
        depth: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnonymousObjectReceiverSource {
    EnclosingInstance { current: bool, depth: u32 },
    ImplicitReceiver { current: bool, depth: u32 },
}

impl AnonymousObjectReceiverSource {
    pub(crate) const fn depth(self) -> u32 {
        match self {
            Self::EnclosingInstance { depth, .. } | Self::ImplicitReceiver { depth, .. } => depth,
        }
    }
}

impl Checker<'_> {
    /// What the implicit receiver `identity` was in source: the enclosing class instance, a context
    /// parameter of its kind, the extension receiver of the named callable declared at `extension`,
    /// or a receiver lambda's.
    pub(super) fn captured_receiver(
        &self,
        scope: &CheckerScope<'_>,
        identity: (usize, usize),
        extension: Option<Span>,
        class_receiver: bool,
    ) -> FirCapturedReceiver {
        if class_receiver {
            return FirCapturedReceiver::Enclosing;
        }
        if let Some(context) = scope.implicit_receiver_context(identity) {
            let kind = match context.kind {
                ContextReceiverKind::Anonymous => CapturedContextKind::Anonymous,
                ContextReceiverKind::FunctionType => CapturedContextKind::FunctionType,
                ContextReceiverKind::LegacyReceiver => CapturedContextKind::LegacyReceiver,
                ContextReceiverKind::Named => unreachable!("a named context value is no receiver"),
            };
            let types = context.types.into_iter().map(|ty| {
                crate::fir::ResolvedTy::new(ty).expect("a context parameter's type is published")
            });
            return FirCapturedReceiver::Context {
                kind,
                types: types.collect(),
                index: u32::try_from(context.index).expect("too many context parameters"),
            };
        }
        let Some(declaration) = extension else {
            let label = scope.implicit_receiver_lambda_label(identity);
            return FirCapturedReceiver::Lambda(label.map(String::into_boxed_str));
        };
        let (index, _) = self
            .extension_receiver_labels
            .iter()
            .rev()
            .find(|(_, candidate)| *candidate == declaration)
            .expect("an extension callable's receiver is labeled while its body is checked");
        let (label, _, _) = &self.this_labels[*index];
        FirCapturedReceiver::Callable(label.clone().into_boxed_str())
    }
}

#[derive(Clone)]
pub(super) struct AnonymousCaptureCandidate {
    pub(super) name: String,
    pub(super) ty: Ty,
    pub(super) shared_cell: bool,
    pub(super) source: AnonymousObjectCaptureSource,
    pub(super) delegate_storage: Option<Ty>,
    /// The candidate is a function parameter or local, not a top-level or class property.
    /// That local keeps its unqualified name inside a nested classifier that redeclares it.
    pub(super) function_local: bool,
    pub(super) receiver_label: Option<Box<str>>,
    pub(super) receiver: Option<crate::fir::FirCapturedReceiver>,
    pub(super) semantic_receiver: Option<AnonymousObjectReceiverSource>,
    /// Exact live checker-scope identity when this candidate is a receiver. It exists only long
    /// enough to project a direct nested anonymous object's use onto this class's capture field.
    pub(super) receiver_identity: Option<(usize, usize)>,
    /// Closure identity assigned from `receiver_identity`. This is the coordinate checked FIR keeps.
    pub(super) receiver_capture: Option<u32>,
}

impl Checker<'_> {
    pub(super) fn refresh_anonymous_receiver_capture_fields(
        &mut self,
        declaration: DeclId,
        candidates: &[AnonymousCaptureCandidate],
    ) {
        let Some(captures) = self.discovered_anonymous_captures.get(&declaration) else {
            self.anonymous_receiver_capture_fields.remove(&declaration);
            return;
        };
        let fields = candidates
            .iter()
            .filter_map(|candidate| {
                let identity = candidate.receiver_identity?;
                let field = captures
                    .iter()
                    .position(|capture| capture.source == candidate.source)?;
                Some((
                    identity,
                    u32::try_from(field).expect("too many anonymous capture fields"),
                ))
            })
            .collect::<HashMap<_, _>>();
        if fields.is_empty() {
            self.anonymous_receiver_capture_fields.remove(&declaration);
        } else {
            self.anonymous_receiver_capture_fields
                .insert(declaration, fields);
        }
    }

    /// Receiver captures a statement-position local class may need, snapshotted before its body
    /// is checked. Each implicit rung is published through [`Checker::implicit_receiver_capture_id`]
    /// so a later superclass prefix and the enclosing field share one closure identity.
    pub(super) fn local_class_receiver_candidates(
        &mut self,
        scope: &CheckerScope<'_>,
    ) -> Vec<ObservedReceiverCapture> {
        let innermost_class = scope.innermost_class_receiver_identity();
        let mut class_receiver_ordinal = 0usize;
        let implicit_receivers = self.implicit_receivers(scope);
        implicit_receivers
            .into_iter()
            // Lexical object/companion singletons are materialized from their published
            // singleton identity and deliberately use `usize::MAX` instead of a scoped
            // receiver coordinate. They are not closure captures.
            .filter(|receiver| receiver.receiver_depth != usize::MAX)
            .map(|receiver| {
                let class_label_identity = receiver
                    .class_receiver
                    .then(|| {
                        let ordinal = class_receiver_ordinal;
                        class_receiver_ordinal += 1;
                        self.this_labels
                            .iter()
                            .enumerate()
                            .rev()
                            .filter(|(_, (_, _, is_class))| *is_class)
                            .nth(ordinal)
                            .map(|(index, _)| super::receiver_label_identity(index))
                    })
                    .flatten();
                let label = self
                    .this_labels
                    .len()
                    .checked_sub(receiver.receiver_depth + 1)
                    .and_then(|index| self.this_labels.get(index))
                    .filter(|(_, _, is_class)| !*is_class)
                    .map(|(label, _, _)| label.clone().into_boxed_str());
                let source = if innermost_class == Some(receiver.identity) {
                    AnonymousObjectCaptureSource::EnclosingInstance {
                        current: receiver.current,
                        depth: u32::try_from(receiver.receiver_depth)
                            .expect("too many implicit receiver rungs"),
                    }
                } else {
                    AnonymousObjectCaptureSource::ImplicitReceiver {
                        current: receiver.current,
                        depth: u32::try_from(receiver.receiver_depth)
                            .expect("too many implicit receiver rungs"),
                    }
                };
                let receiver_capture =
                    self.implicit_receiver_capture_id(receiver.class_receiver, receiver.identity);
                let capture = AnonymousObjectCapture {
                    name: if matches!(
                        source,
                        AnonymousObjectCaptureSource::EnclosingInstance { .. }
                    ) {
                        "this$0".to_string()
                    } else if receiver.current {
                        "this$receiver".to_string()
                    } else {
                        format!("this$receiver${}", receiver.receiver_depth)
                    },
                    ty: receiver.ty,
                    shared_cell: false,
                    storage_ty: None,
                    source,
                    receiver_label: label,
                    receiver: Some(self.captured_receiver(
                        scope,
                        receiver.identity,
                        receiver.extension_receiver,
                        receiver.class_receiver,
                    )),
                    semantic_receiver: Some(
                        if matches!(
                            source,
                            AnonymousObjectCaptureSource::EnclosingInstance { .. }
                        ) {
                            AnonymousObjectReceiverSource::EnclosingInstance {
                                current: receiver.current,
                                depth: u32::try_from(receiver.receiver_depth)
                                    .expect("too many implicit receiver rungs"),
                            }
                        } else {
                            AnonymousObjectReceiverSource::ImplicitReceiver {
                                current: receiver.current,
                                depth: u32::try_from(receiver.receiver_depth)
                                    .expect("too many implicit receiver rungs"),
                            }
                        },
                    ),
                    lexical_shadow_depth: 0,
                    capture_dependency: None,
                    receiver_capture,
                };
                let mut identities = vec![receiver.identity];
                if let Some(identity) = class_label_identity {
                    if identity != receiver.identity {
                        identities.push(identity);
                    }
                }
                let uses_before = identities
                    .into_iter()
                    .map(|identity| {
                        (
                            identity,
                            self.implicit_receiver_identity_use_count(identity),
                        )
                    })
                    .collect();
                ObservedReceiverCapture {
                    capture,
                    uses_before,
                }
            })
            .collect()
    }
}

pub(super) struct ObservedReceiverCapture {
    pub(super) capture: AnonymousObjectCapture,
    pub(super) uses_before: Vec<((usize, usize), usize)>,
}

/// Keep one capture per semantic source. A capture already recorded for that source still
/// receives the closure id when publication of the local-class inventory ran first.
pub(super) fn merge_local_receiver_capture(
    captures: &mut Vec<AnonymousObjectCapture>,
    bindings: &mut Vec<Option<u32>>,
    candidate: AnonymousObjectCapture,
) {
    if let Some(existing) = captures
        .iter_mut()
        .find(|existing| existing.source == candidate.source)
    {
        if existing.receiver_capture.is_none() {
            existing.receiver_capture = candidate.receiver_capture;
        }
        return;
    }
    captures.push(candidate);
    bindings.push(None);
}

/// Drop an implicit receiver the local-class inventory held only so the body check could see it.
///
/// A receiver whose use count increased stays, in the field the body check already numbered.
/// One the body never read is not a constructor parameter: kotlinc omits it, and a constructor
/// reference would otherwise take the receiver as its first value.
pub(super) fn drop_unused_implicit_receivers(
    captures: &mut Vec<AnonymousObjectCapture>,
    bindings: &mut Vec<Option<u32>>,
    used: &[(usize, AnonymousObjectCaptureSource)],
) {
    let mut kept_captures = Vec::with_capacity(captures.len());
    let mut kept_bindings = Vec::with_capacity(bindings.len());
    for (capture, binding) in captures.drain(..).zip(bindings.drain(..)) {
        let unused_receiver = matches!(
            capture.source,
            AnonymousObjectCaptureSource::ImplicitReceiver { .. }
        ) && !used.iter().any(|(_, source)| *source == capture.source);
        if unused_receiver {
            continue;
        }
        kept_captures.push(capture);
        kept_bindings.push(binding);
    }
    *captures = kept_captures;
    *bindings = kept_bindings;
}

#[derive(Clone, Copy)]
pub(super) struct SelectedLocalCallableCaptures<'a> {
    pub(super) calls: &'a HashMap<ExprId, ResolvedCall>,
    pub(super) expressions: &'a HashMap<ExprId, ExprLowering>,
    pub(super) statements: &'a HashMap<StmtId, StmtLowering>,
}

fn expression_uses_selected_local_callable_capture(
    file: &File,
    expression: ExprId,
    name: &str,
    selected: SelectedLocalCallableCaptures<'_>,
) -> bool {
    let statement = selected
        .calls
        .get(&expression)
        .and_then(|call| match call {
            ResolvedCall::LocalFunction(call) => Some(call.stmt_id),
            _ => None,
        })
        .or_else(|| match selected.expressions.get(&expression) {
            Some(ExprLowering::LocalFunction { stmt_id, .. }) => Some(*stmt_id),
            _ => None,
        });
    if statement.is_some_and(|statement| {
        matches!(
            selected.statements.get(&statement),
            Some(StmtLowering::LocalFunction(function))
                if function.captures.iter().any(|capture| capture.name == name)
        )
    }) {
        return true;
    }
    let mut expressions = Vec::new();
    let mut statements = Vec::new();
    file.any_child_expr(
        expression,
        &mut |child| {
            expressions.push(child);
            false
        },
        &mut |statement| {
            statements.push(statement);
            false
        },
    );
    expressions
        .into_iter()
        .any(|child| expression_uses_selected_local_callable_capture(file, child, name, selected))
        || statements.into_iter().any(|statement| {
            let mut children = Vec::new();
            file.any_child_stmt(statement, &mut |child| {
                children.push(child);
                false
            });
            children.into_iter().any(|child| {
                expression_uses_selected_local_callable_capture(file, child, name, selected)
            })
        })
}

fn anonymous_descendant_uses_selected_local_callable_capture(
    file: &File,
    declaration: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    name: &str,
    selected: SelectedLocalCallableCaptures<'_>,
) -> bool {
    anonymous_descendants(declaration, lexical_scope).any(|candidate| {
        anonymous_body_expressions(file, candidate)
            .into_iter()
            .any(|expression| {
                expression_uses_selected_local_callable_capture(file, expression, name, selected)
            })
    })
}

pub(super) fn record_anonymous_construction_captures(
    file: &File,
    construction: ExprId,
    lexical_scope: &AnonymousLexicalClassScope,
    candidates: &[AnonymousCaptureCandidate],
    preserve_missing: bool,
    selected_local_callables: SelectedLocalCallableCaptures<'_>,
    captures: &mut HashMap<DeclId, Vec<AnonymousObjectCapture>>,
) -> Vec<Option<u32>> {
    let Some(&declaration) = file.anonymous_object_classes.get(&construction) else {
        return Vec::new();
    };
    crate::trace_compiler!(
        "resolve",
        "anonymous capture candidates declaration={declaration:?} construction={construction:?} candidates={:?}",
        candidates
            .iter()
            .map(|candidate| (&candidate.name, candidate.ty, candidate.delegate_storage))
            .collect::<Vec<_>>(),
    );
    // Match ordinary name lookup for name-addressed captures: `visit_bindings` is innermost-first,
    // so retain the first lexical/storage candidate for each spelling. Direct receiver captures
    // are coordinate-addressed scope-tower rungs; a receiver already forwarded through class
    // storage remains name-addressed here even though it also retains its semantic coordinate for
    // a later super-argument rematerialization.
    let selected = candidates
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            matches!(
                candidate.source,
                AnonymousObjectCaptureSource::EnclosingInstance { .. }
                    | AnonymousObjectCaptureSource::ImplicitReceiver { .. }
            ) || !candidates[..*index]
                .iter()
                .any(|earlier| earlier.name == candidate.name)
        })
        .filter(|(_, candidate)| candidate.ty != Ty::Error)
        .filter(|(_, candidate)| match candidate.source {
            // Enclosing-instance need was decided against the anonymous body's receiver uses.
            // It has no source variable name, so lexical bound/write/use filters do not apply.
            AnonymousObjectCaptureSource::EnclosingInstance { .. }
            | AnonymousObjectCaptureSource::ImplicitReceiver { .. }
            | AnonymousObjectCaptureSource::ClassStorage { .. } => true,
            AnonymousObjectCaptureSource::LexicalValue => {
                enclosing_value_visible_beside_member(
                    file,
                    declaration,
                    &candidate.name,
                    candidate.function_local,
                ) && (candidate.delegate_storage.is_some()
                    || anonymous_descendant_uses_name(
                        file,
                        declaration,
                        lexical_scope,
                        &candidate.name,
                        candidate.ty,
                        candidate.function_local,
                    )
                    || anonymous_descendant_uses_selected_local_callable_capture(
                        file,
                        declaration,
                        lexical_scope,
                        &candidate.name,
                        selected_local_callables,
                    )
                    || anonymous_descendant_writes_name(
                        file,
                        declaration,
                        lexical_scope,
                        &candidate.name,
                        candidate.function_local,
                    ))
            }
        })
        .map(|(_, candidate)| AnonymousObjectCapture {
            name: candidate.name.clone(),
            ty: candidate.ty,
            shared_cell: candidate.shared_cell,
            storage_ty: candidate.delegate_storage,
            source: candidate.source,
            receiver_label: candidate.receiver_label.clone(),
            receiver: candidate.receiver.clone(),
            semantic_receiver: candidate.semantic_receiver,
            lexical_shadow_depth: 0,
            capture_dependency: None,
            receiver_capture: candidate.receiver_capture,
        })
        .collect::<Vec<_>>();
    crate::trace_compiler!(
        "resolve",
        "anonymous capture selection declaration={declaration:?} captures={selected:?}",
    );
    let (selected, field_remap) = capture_field_order::reconcile(
        captures.get(&declaration).map(Vec::as_slice),
        selected,
        preserve_missing,
    );
    crate::trace_compiler!(
        "resolve",
        "anonymous captures selected declaration={declaration:?} captures={selected:?}",
    );
    captures.insert(declaration, selected);
    field_remap
}
