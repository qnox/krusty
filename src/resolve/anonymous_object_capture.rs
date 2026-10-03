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
    /// Stable declaration/classifier role used to find the same semantic receiver after a scope
    /// rebuild. Enclosing instances carry a role but deliberately have no closure id above.
    pub(crate) receiver_role: Option<super::ReceiverDeclarationRole>,
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
        let (label, _, _, _) = &self.this_labels[*index];
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
    /// Closure identity assigned from the recorded declaration role, not the transient scope.
    pub(super) receiver_capture: Option<u32>,
    pub(super) receiver_role: Option<super::ReceiverDeclarationRole>,
}

fn receiver_capture_source(
    class_receiver: bool,
    current: bool,
    depth: u32,
) -> AnonymousObjectCaptureSource {
    if class_receiver {
        AnonymousObjectCaptureSource::EnclosingInstance { current, depth }
    } else {
        AnonymousObjectCaptureSource::ImplicitReceiver { current, depth }
    }
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
                            .filter(|(_, (_, _, is_class, _))| *is_class)
                            .nth(ordinal)
                            .map(|(index, _)| super::receiver_label_identity(index))
                    })
                    .flatten();
                let label = self
                    .this_labels
                    .len()
                    .checked_sub(receiver.receiver_depth + 1)
                    .and_then(|index| self.this_labels.get(index))
                    .filter(|(_, _, is_class, _)| !*is_class)
                    .map(|(label, _, _, _)| label.clone().into_boxed_str());
                let source = receiver_capture_source(
                    receiver.class_receiver,
                    receiver.current,
                    u32::try_from(receiver.receiver_depth)
                        .expect("too many implicit receiver rungs"),
                );
                let receiver_capture = self
                    .implicit_receiver_capture_id(receiver.class_receiver, receiver.receiver_role);
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
                    receiver_role: receiver.receiver_role,
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
    capture: AnonymousObjectCapture,
    uses_before: Vec<((usize, usize), usize)>,
}

/// Proven selections of one declaration, separate from the temporary full receiver inventory
/// installed while its body is checked. Method memoization survives inference revisits.
pub(super) struct LocalClassCaptureInventory {
    captures: Vec<AnonymousObjectCapture>,
    finalized: bool,
}

/// Keep one capture per semantic source. A capture already recorded for that source still
/// receives the closure id when publication of the local-class inventory ran first.
fn merge_local_receiver_capture(
    captures: &mut Vec<AnonymousObjectCapture>,
    bindings: &mut Vec<Option<u32>>,
    candidate: AnonymousObjectCapture,
) {
    if let Some(existing) = captures
        .iter_mut()
        .find(|existing| existing.source == candidate.source)
    {
        if existing.receiver_role.is_none() && candidate.receiver_role.is_some() {
            existing.receiver_capture = candidate.receiver_capture;
            existing.receiver_role = candidate.receiver_role;
        } else if candidate.receiver_role.is_some() {
            assert_eq!(existing.receiver_capture, candidate.receiver_capture);
            assert_eq!(existing.receiver_role, candidate.receiver_role);
        }
        return;
    }
    captures.push(candidate);
    bindings.push(None);
}

impl Checker<'_> {
    pub(super) fn local_class_capture_inventory(
        &self,
        declaration: DeclId,
    ) -> LocalClassCaptureInventory {
        LocalClassCaptureInventory {
            captures: self
                .discovered_local_class_captures
                .get(&declaration)
                .cloned()
                .unwrap_or_default(),
            finalized: self.finalized_local_class_captures.contains(&declaration),
        }
    }

    /// Refresh a prior proven receiver from this declaration's current lexical inventory. Source
    /// coordinates can change when constructor/default scopes are rebuilt: the recorded receiver
    /// declaration role owns the remap. Enclosing instances use their stable classifier role and
    /// remain outside the closure-id namespace.
    pub(super) fn restore_proven_local_receiver_captures(
        &self,
        established: &mut LocalClassCaptureInventory,
        candidates: &[ObservedReceiverCapture],
        captures: &mut Vec<AnonymousObjectCapture>,
        bindings: &mut Vec<Option<u32>>,
    ) {
        let established_finalized = established.finalized;
        for prior in &mut established.captures {
            let declaration_owned_receiver = matches!(
                prior.source,
                AnonymousObjectCaptureSource::ImplicitReceiver { .. }
            ) || (matches!(
                prior.source,
                AnonymousObjectCaptureSource::EnclosingInstance { .. }
            ) && prior.receiver_role.is_some());
            if !declaration_owned_receiver {
                continue;
            }
            let current =
                refreshed_local_receiver_capture(prior, candidates, established_finalized);
            // The proof remains declaration-owned, but its source operand belongs to this visit's
            // reconstructed tower. Keep established exact types for pending-type reconciliation.
            prior.source = current.source;
            prior.semantic_receiver = current.semantic_receiver;
            prior.name = current.name.clone();
            merge_local_receiver_capture(captures, bindings, current.clone());
            let selected = captures
                .iter_mut()
                .find(|capture| capture.source == prior.source)
                .expect("the refreshed receiver was installed");
            *selected = current;
        }
    }

    /// Finish a local class's receiver captures after its body has been checked.
    ///
    /// Receivers the body read are ordered by first use. An implicit receiver the inventory held
    /// only so that check could see it, and which the body never read, is dropped: it is not a
    /// constructor parameter. The discovered-map entry is replaced with that result, or removed
    /// when nothing remains.
    pub(super) fn finalize_local_class_receiver_captures(
        &mut self,
        declaration: DeclId,
        captures: &mut Vec<AnonymousObjectCapture>,
        bindings: &mut Vec<Option<u32>>,
        receiver_candidates: Vec<ObservedReceiverCapture>,
        uses_before_body: usize,
        established: LocalClassCaptureInventory,
    ) {
        let mut used_receivers = Vec::new();
        for observed in &receiver_candidates {
            let Some(first_use) = observed
                .uses_before
                .iter()
                .filter(|(identity, before)| {
                    self.implicit_receiver_identity_use_count(*identity) > *before
                })
                .map(|(identity, _)| {
                    self.implicit_receiver_identity_uses.first_use_since(
                        uses_before_body,
                        *identity,
                        None,
                    )
                })
                .min()
            else {
                continue;
            };
            used_receivers.push((first_use, observed.capture.source));
            merge_local_receiver_capture(captures, bindings, observed.capture.clone());
        }
        // An inner class is not entered as its own local-class statement, so it never publishes
        // captures of its own. Its super call reads a superclass receiver from the enclosing
        // local class. Keep that receiver here even when this class's body never mentions it.
        self.retain_receivers_read_by_nested_inner_superclasses(
            declaration,
            captures,
            bindings,
            &receiver_candidates,
        );
        // Provisional visits publish only proven selections, but cannot mark their types final.
        let provisional = self.postponed_argument_depth != 0
            || captures.iter().any(|capture| {
                capture.ty.mentions_pending()
                    || capture
                        .storage_ty
                        .is_some_and(|storage| storage.mentions_pending())
            });
        // Established fields are proven reads, not the temporary candidate overlay. A completed
        // local method never selects its receiver again during a memoized revisit.
        let mut proven_uses = established
            .captures
            .iter()
            .filter_map(|capture| {
                matches!(
                    capture.source,
                    AnonymousObjectCaptureSource::ImplicitReceiver { .. }
                        | AnonymousObjectCaptureSource::EnclosingInstance { .. }
                )
                .then_some(capture.source)
            })
            .enumerate()
            .collect::<Vec<_>>();
        used_receivers.sort_by_key(|(position, _)| *position);
        for (_, source) in used_receivers {
            if !proven_uses.iter().any(|(_, proven)| *proven == source) {
                proven_uses.push((proven_uses.len(), source));
            }
        }
        let used_receivers = proven_uses;
        capture_field_order::order_receivers_by_first_use(
            captures,
            bindings,
            used_receivers.clone(),
        );
        // Publish only selected inputs, including on provisional visits. The full overlay was
        // needed for checking, but conserving it would give Unused a phantom constructor input.
        drop_unused_implicit_receivers(captures, bindings, &used_receivers);
        let (selected, _) = capture_field_order::reconcile(
            Some(&established.captures),
            std::mem::take(captures),
            false,
        );
        *captures = selected;
        if !provisional || established.finalized {
            self.finalized_local_class_captures.insert(declaration);
        }
        assert_eq!(
            captures.len(),
            bindings.len(),
            "local capture publication pairs every selected field with its binding identity"
        );
        if captures.is_empty() {
            self.discovered_local_class_captures.remove(&declaration);
            self.discovered_local_class_capture_bindings
                .remove(&declaration);
        } else {
            self.discovered_local_class_captures
                .insert(declaration, std::mem::take(captures));
            self.discovered_local_class_capture_bindings
                .insert(declaration, std::mem::take(bindings));
        }
    }

    /// Keep an implicit receiver a direct inner subclass's superclass constructor still needs.
    ///
    /// The inner constructor's only prefix is the enclosing instance. Lowering reads the superclass
    /// capture from that instance by closure identity, so this class has to keep the same receiver
    /// the superclass captured. A further-nested inner class reads its own enclosing class, not this
    /// one, and a non-inner subclass carries the receiver on its own constructor.
    ///
    /// The initial inventory records only the nearest receiver. A superclass can capture a further
    /// rung (`this@bar` while this class is declared in a `Scope.() ->` lambda). That closure
    /// identity is already in this declaration's receiver tower; publish that tower entry before
    /// forwarding it. The superclass coordinate is not reused: a nearer lambda receiver shifts it.
    fn retain_receivers_read_by_nested_inner_superclasses(
        &self,
        declaration: DeclId,
        captures: &mut Vec<AnonymousObjectCapture>,
        bindings: &mut Vec<Option<u32>>,
        candidates: &[ObservedReceiverCapture],
    ) {
        let crate::ast::Decl::Class(class) = self.file.decl(declaration) else {
            return;
        };
        let Some(enclosing) = self.active_classifier_internal(declaration, class) else {
            return;
        };
        let Some(statement) = self
            .file
            .local_class_decls
            .iter()
            .find_map(|(statement, owner)| (*owner == declaration).then_some(*statement))
        else {
            return;
        };
        let Some(nested) = self.file.local_class_nested.get(&statement).cloned() else {
            return;
        };
        let mut required = Vec::new();
        for nested in nested {
            let crate::ast::Decl::Class(nested_class) = self.file.decl(nested) else {
                continue;
            };
            if nested_class.inner_of.is_none() {
                continue;
            }
            let Some(nested_owner) = self.active_classifier_internal(nested, nested_class) else {
                continue;
            };
            if nested_owner.nested_owner() != Some(enclosing) {
                continue;
            }
            let Some(superclass) = self
                .resolved_body_local_supertypes
                .get(&nested_owner)
                .and_then(|supertypes| supertypes.first())
                .and_then(|supertype| supertype.kotlin_class_internal())
            else {
                continue;
            };
            let Some(superclass_captures) = self.discovered_captures_of(superclass) else {
                continue;
            };
            required.extend(superclass_captures.iter().filter_map(|capture| {
                if !matches!(
                    capture.source,
                    AnonymousObjectCaptureSource::ImplicitReceiver { .. }
                ) {
                    return None;
                }
                // An enclosing class instance has a field identity and is supplied by the
                // ordinary local-capture dependency path. This rule is only for a receiver rung
                // whose exact cross-class identity was published by the checker.
                let receiver = capture.receiver_capture?;
                Some((receiver, capture.capture_dependency))
            }));
        }
        for (receiver, dependency) in required {
            if !captures
                .iter()
                .any(|local| local.receiver_capture == Some(receiver))
            {
                let published = candidates
                    .iter()
                    .find(|candidate| candidate.capture.receiver_capture == Some(receiver))
                    .expect(
                        "an enclosing local class's receiver tower contains the identity its \
                         inner subclass forwards",
                    );
                merge_local_receiver_capture(captures, bindings, published.capture.clone());
            }
            let local = captures
                .iter_mut()
                .find(|local| local.receiver_capture == Some(receiver))
                .expect(
                    "an enclosing local class publishes the receiver identity required by its \
                     inner subclass",
                );
            let dependency =
                dependency.unwrap_or(crate::fir::ClassCaptureIdentity::Receiver(receiver));
            if let Some(existing) = local.capture_dependency {
                assert_eq!(
                    existing, dependency,
                    "one receiver identity cannot forward two closure fields"
                );
            } else {
                local.capture_dependency = Some(dependency);
            }
        }
    }

    fn discovered_captures_of(
        &self,
        owner: crate::types::TypeName,
    ) -> Option<&Vec<AnonymousObjectCapture>> {
        let declaration =
            self.discovered_local_class_captures
                .keys()
                .copied()
                .find(|declaration| {
                    matches!(
                        self.file.decl(*declaration),
                        crate::ast::Decl::Class(class)
                            if self.active_classifier_internal(*declaration, class) == Some(owner)
                    )
                })?;
        self.discovered_local_class_captures.get(&declaration)
    }
}

fn refreshed_local_receiver_capture(
    prior: &AnonymousObjectCapture,
    candidates: &[ObservedReceiverCapture],
    established_finalized: bool,
) -> AnonymousObjectCapture {
    let role = prior
        .receiver_role
        .expect("a proven implicit receiver has recorded declaration provenance");
    let Some(mut current) = candidates
        .iter()
        .find(|candidate| candidate.capture.receiver_role == Some(role))
        .map(|candidate| candidate.capture.clone())
    else {
        assert!(
            established_finalized,
            "a provisional local-class receiver remains in its declaration's lexical tower"
        );
        // A memoized revisit may not reconstruct an already-completed inline call's transient
        // receiver scope. The finalized inventory is the authoritative checked decision from the
        // declaration's construction site; retain that exact role, coordinate and storage source
        // instead of matching another live receiver by depth or type.
        return prior.clone();
    };
    match prior.source {
        AnonymousObjectCaptureSource::ImplicitReceiver { .. } => {
            let identity = prior
                .receiver_capture
                .expect("a proven implicit receiver has a declaration-owned capture identity");
            assert_eq!(current.receiver_capture, Some(identity));
        }
        AnonymousObjectCaptureSource::EnclosingInstance { .. } => {
            assert_eq!(prior.receiver_capture, None);
            assert_eq!(current.receiver_capture, None);
        }
        AnonymousObjectCaptureSource::LexicalValue
        | AnonymousObjectCaptureSource::ClassStorage { .. } => {
            panic!("only receiver captures are refreshed")
        }
    }
    current.capture_dependency = prior.capture_dependency;
    current
}

#[cfg(test)]
mod receiver_remap_tests {
    use super::*;
    use crate::resolve::receiver_capture_identity::ReceiverCaptureIds;
    use crate::resolve::scope::{Scope, ScopeKind};

    fn receiver(
        identity: u32,
        role: super::super::ReceiverDeclarationRole,
        depth: u32,
    ) -> AnonymousObjectCapture {
        AnonymousObjectCapture {
            name: "captured".into(),
            ty: Ty::obj("CaptureToken"),
            shared_cell: false,
            storage_ty: None,
            source: AnonymousObjectCaptureSource::ImplicitReceiver {
                current: depth == 0,
                depth,
            },
            receiver_label: None,
            receiver: Some(FirCapturedReceiver::Lambda(None)),
            semantic_receiver: Some(AnonymousObjectReceiverSource::ImplicitReceiver {
                current: depth == 0,
                depth,
            }),
            lexical_shadow_depth: 0,
            capture_dependency: None,
            receiver_capture: Some(identity),
            receiver_role: Some(role),
        }
    }

    #[test]
    fn constructor_scope_remap_selects_identity_not_old_coordinate_or_same_type() {
        let root: Scope<'_, ()> = Scope::root();
        let source_lambda = crate::diag::Span { lo: 7, hi: 19 };
        let first = root
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(source_lambda);
        let rebuilt = root
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(source_lambda);
        let shifted_scope = rebuilt.child(ScopeKind::Class {
            ty: Ty::obj("CaptureHost"),
            carries_outer: true,
        });
        let first_identity = first.implicit_receivers_with_declarations()[0].2;
        let shifted_identity = shifted_scope.implicit_receivers_with_declarations()[1].2;
        assert_ne!(first_identity, shifted_identity);
        let role = first
            .implicit_receiver_role(first_identity)
            .expect("recorded source lambda");
        let shifted_role = shifted_scope
            .implicit_receiver_role(shifted_identity)
            .expect("rebuilt source lambda");
        let mut ids = ReceiverCaptureIds::default();
        let prior_id = ids.id(role);
        let shifted_id = ids.id(shifted_role);
        let mut prior = receiver(prior_id, role, 0);
        prior.capture_dependency = Some(crate::fir::ClassCaptureIdentity::Receiver(prior_id));
        let shifted = receiver(shifted_id, shifted_role, 1);
        let wrong_scope = root
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(crate::diag::Span { lo: 20, hi: 32 });
        let wrong_identity = wrong_scope.implicit_receivers_with_declarations()[0].2;
        let wrong_role = wrong_scope
            .implicit_receiver_role(wrong_identity)
            .expect("distinct source lambda");
        let wrong_id = ids.id(wrong_role);
        let candidates = [
            ObservedReceiverCapture {
                capture: receiver(wrong_id, wrong_role, 0),
                uses_before: Vec::new(),
            },
            ObservedReceiverCapture {
                capture: shifted.clone(),
                uses_before: Vec::new(),
            },
        ];
        let current = refreshed_local_receiver_capture(&prior, &candidates, false);
        assert_eq!(current.receiver_capture, Some(prior_id));
        assert_eq!(current.receiver_role, Some(role));
        assert_eq!(current.source, shifted.source);
        assert_eq!(current.semantic_receiver, shifted.semantic_receiver);
        assert_eq!(current.capture_dependency, prior.capture_dependency);
        assert_eq!(current.ty, shifted.ty);
    }

    #[test]
    fn a_coordinate_without_receiver_provenance_cannot_remap_a_proven_selection() {
        let role = super::super::ReceiverDeclarationRole::Lambda {
            expression: crate::diag::Span { lo: 7, hi: 19 },
            slot: super::super::LambdaReceiverSlot::Extension,
        };
        let identity = ReceiverCaptureIds::default().id(role);
        let mut prior = receiver(identity, role, 0);
        prior.receiver_role = None;
        prior.receiver_capture = None;
        let candidate = ObservedReceiverCapture {
            capture: prior.clone(),
            uses_before: Vec::new(),
        };
        let failure = std::panic::catch_unwind(|| {
            refreshed_local_receiver_capture(&prior, &[candidate], false)
        })
        .expect_err("a matching old coordinate does not prove receiver identity");
        assert_eq!(
            failure
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| failure.downcast_ref::<&str>().copied()),
            Some("a proven implicit receiver has recorded declaration provenance")
        );
    }

    #[test]
    fn every_class_receiver_rung_is_an_enclosing_instance_not_a_closure() {
        let root: Scope<'_, ()> = Scope::root();
        let outer = root.child(ScopeKind::Class {
            ty: Ty::obj("CaptureOuter"),
            carries_outer: false,
        });
        let inner = outer.child(ScopeKind::Class {
            ty: Ty::obj("CaptureInner"),
            carries_outer: true,
        });
        let lambda = inner
            .function_child(Some(Ty::obj("CaptureToken")), None, &[])
            .with_lambda_expression(crate::diag::Span { lo: 7, hi: 19 });
        let mut ids = ReceiverCaptureIds::default();
        let classification = lambda
            .implicit_receivers_with_declarations()
            .into_iter()
            .enumerate()
            .map(|(depth, (_, _, identity, class_receiver))| {
                (
                    receiver_capture_source(class_receiver, depth == 0, depth as u32),
                    ids.capture_id(class_receiver, lambda.implicit_receiver_role(identity)),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            classification,
            vec![
                (
                    AnonymousObjectCaptureSource::ImplicitReceiver {
                        current: true,
                        depth: 0,
                    },
                    Some(0),
                ),
                (
                    AnonymousObjectCaptureSource::EnclosingInstance {
                        current: false,
                        depth: 1,
                    },
                    None,
                ),
                (
                    AnonymousObjectCaptureSource::EnclosingInstance {
                        current: false,
                        depth: 2,
                    },
                    None,
                ),
            ]
        );
    }

    #[test]
    fn a_proven_enclosing_instance_remaps_by_classifier_not_old_depth() {
        let outer = crate::types::type_name("test/CaptureOuter");
        let other = crate::types::type_name("test/OtherOuter");
        let outer_role = super::super::ReceiverDeclarationRole::EnclosingClass(outer);
        let other_role = super::super::ReceiverDeclarationRole::EnclosingClass(other);
        let mut prior = receiver(0, outer_role, 1);
        prior.source = AnonymousObjectCaptureSource::EnclosingInstance {
            current: false,
            depth: 1,
        };
        prior.semantic_receiver = Some(AnonymousObjectReceiverSource::EnclosingInstance {
            current: false,
            depth: 1,
        });
        prior.receiver_capture = None;
        let mut wrong = prior.clone();
        wrong.receiver_role = Some(other_role);
        let mut shifted = prior.clone();
        shifted.source = AnonymousObjectCaptureSource::EnclosingInstance {
            current: false,
            depth: 2,
        };
        shifted.semantic_receiver = Some(AnonymousObjectReceiverSource::EnclosingInstance {
            current: false,
            depth: 2,
        });
        let current = refreshed_local_receiver_capture(
            &prior,
            &[
                ObservedReceiverCapture {
                    capture: wrong,
                    uses_before: Vec::new(),
                },
                ObservedReceiverCapture {
                    capture: shifted.clone(),
                    uses_before: Vec::new(),
                },
            ],
            false,
        );
        assert_eq!(current.receiver_role, Some(outer_role));
        assert_eq!(current.source, shifted.source);
        assert_eq!(current.receiver_capture, None);
    }

    #[test]
    fn a_finalized_receiver_survives_a_memoized_visit_without_transient_candidates() {
        let role = super::super::ReceiverDeclarationRole::Lambda {
            expression: crate::diag::Span { lo: 7, hi: 19 },
            slot: super::super::LambdaReceiverSlot::Extension,
        };
        let identity = ReceiverCaptureIds::default().id(role);
        let mut prior = receiver(identity, role, 1);
        prior.capture_dependency = Some(crate::fir::ClassCaptureIdentity::Receiver(identity));

        assert_eq!(refreshed_local_receiver_capture(&prior, &[], true), prior);
    }
}

/// Drop an implicit receiver the local-class inventory held only so the body check could see it.
///
/// A receiver whose use count increased stays, in the field the body check already numbered.
/// One the body never read is not a constructor parameter: kotlinc omits it, and a constructor
/// reference would otherwise take the receiver as its first value. A receiver a superclass
/// constructor still requires stays too: the subclass may never read it, but it must pass it on.
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
        ) && capture.capture_dependency.is_none()
            && !used.iter().any(|(_, source)| *source == capture.source);
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
            receiver_role: candidate.receiver_role,
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
