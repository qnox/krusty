//! Checked local-class declaration and lexical-capture identities.

use super::enclosing_capture_owners::CheckedClassCaptureSource;
use super::*;
use crate::ast::DeclId;
use crate::fir::FirAnonymousSuperArgument;
use crate::resolve::{AnonymousObjectCaptureSource, AnonymousObjectReceiverSource};

#[derive(Clone)]
enum ConstructorCaptureOperand {
    Class(ClassCaptureBinding),
    Local(LocalBinding),
    Captured {
        enclosing_depth: u32,
        binding: LocalBinding,
    },
    ImplicitReceiver(crate::resolve::ImplicitReceiverSelection),
}

/// The storage receiver for an outer-class capture: the receiver expression plus the capture path
/// that remains to be walked from it.
type StorageReceiver = Result<Option<(FirExprId, Box<[DeclarationId]>)>, BodyCheckFailure>;

impl BodyFirChecker<'_> {
    pub(super) fn binding_source_at_shadow_depth(
        &self,
        name: &str,
        shadow_depth: usize,
    ) -> Option<(Option<u32>, LocalBinding)> {
        let mut seen = crate::fx_hash::HashSet::default();
        self.scopes
            .iter()
            .rev()
            .filter_map(|scope| scope.get(name).copied().map(|binding| (None, binding)))
            .chain(
                self.outer_values
                    .shadowed(name)
                    .map(|(depth, binding)| (Some(*depth), *binding)),
            )
            .filter(|(depth, binding)| seen.insert((*depth, binding.value)))
            .nth(shadow_depth)
    }

    /// A construction-site super forward cannot use the anonymous constructor's capture field.
    pub(super) fn active_class_storage_read(
        &self,
        expression: ExprId,
    ) -> Result<Option<ClassCaptureBinding>, BodyCheckFailure> {
        if self.hoist_anonymous_super_argument {
            return Ok(None);
        }
        let selected = match self.info.expr_lowers.get(&expression) {
            Some(
                selected @ (ExprLowering::ClassStorageRead(_)
                | ExprLowering::EnumEntryStorageRead { .. }),
            ) => selected,
            _ => return Ok(None),
        };
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let binding = match selected {
            ExprLowering::ClassStorageRead(storage) => {
                self.selected_class_capture_binding(*storage, span)?
            }
            ExprLowering::EnumEntryStorageRead { owner, field } => self
                .enum_entry_storage_binding(
                    *owner,
                    *field,
                    self.expression_type(expression)?,
                    span,
                )?,
            _ => unreachable!("matched storage lowering"),
        };
        Ok(Some(binding))
    }

    pub(super) fn selected_storage_owner(
        &self,
        storage: crate::resolve::ClassStorageSource,
        span: Option<Span>,
    ) -> Result<DeclarationId, BodyCheckFailure> {
        self.index
            .classifier_declaration(storage.owner)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))
    }

    pub(super) fn selected_enum_entry_storage_owner(
        &self,
        expected: crate::types::TypeName,
        field: u32,
        span: Option<Span>,
    ) -> Result<DeclarationId, BodyCheckFailure> {
        let property = self
            .enum_entry_property(field)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let declaration = self
            .index
            .property_declaration(property)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        let owner = self
            .index
            .declaration_anchor(declaration)
            .and_then(|anchor| anchor.owner)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget))?;
        if !crate::resolve::enum_entry_storage_owner_matches(self.index, owner, expected)
            || self
                .index
                .declaration_anchor(owner)
                .map(|anchor| anchor.kind)
                != Some(DeclarationKind::EnumEntry)
            || self
                .index
                .owned_declaration(owner, DeclarationKind::Property, field)
                != Some(declaration)
        {
            return Err(self.failure(span, BodyCheckFailureKind::MissingStablePropertyTarget));
        }
        Ok(owner)
    }

    pub(super) fn enum_entry_storage_binding(
        &self,
        expected: crate::types::TypeName,
        field: u32,
        ty: ResolvedTy,
        span: Span,
    ) -> Result<ClassCaptureBinding, BodyCheckFailure> {
        let owner = self.selected_enum_entry_storage_owner(expected, field, Some(span))?;
        match self.selected_storage_capture_binding(owner, field, span)? {
            Some(binding) => Ok(binding),
            None => self.declared_class_storage_binding(owner, field, ty),
        }
    }

    pub(super) fn selected_class_capture_binding(
        &self,
        storage: crate::resolve::ClassStorageSource,
        span: Span,
    ) -> Result<ClassCaptureBinding, BodyCheckFailure> {
        self.selected_storage_owner(storage, Some(span))?;
        let binding = self
            .class_capture_binding(storage.identity)
            .ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
        Ok(binding)
    }

    /// Publish the capture prefix for a local-class construction that crosses a streamed body
    /// boundary. Inside the declaring body the checked `LocalDeclaration` already owns the
    /// capture sources. A member of that class (or a nested class) instead sees the same captures
    /// through stable `(owner, field)` identities and must carry those reads on the call itself.
    pub(super) fn external_constructor_capture_arguments(
        &mut self,
        constructor: DeclarationId,
        origin: OriginId,
        span: Span,
        plan: Option<&crate::resolve::ConstructorCapturePlan>,
    ) -> Result<Option<Box<[FirConstructorCaptureArgument]>>, BodyCheckFailure> {
        let Some(classifier) = self.index.enclosing_classifier(constructor) else {
            return Ok(None);
        };
        if self.body.owns_class_body_context(classifier.declaration) {
            return Ok(None);
        }
        if !self
            .index
            .declaration_header(classifier.declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
        {
            return Ok(None);
        }
        let context = self
            .session
            .class_bodies
            .get(&classifier.declaration)
            .cloned()
            .ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
        let plan = plan.ok_or_else(|| {
            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
        })?;
        let value_captures = if context.capture_values.is_empty() {
            context.values.into_iter().collect::<Vec<_>>()
        } else {
            context.capture_values
        };
        let mut required =
            value_captures
                .into_iter()
                .map(|(_, binding)| binding)
                .chain(context.delegates.into_values().filter_map(
                    |delegate| match delegate.storage {
                        DelegateStorage::ClassField(binding) => Some(binding),
                        _ => None,
                    },
                ))
                .chain(context.receivers)
                .collect::<Vec<_>>();
        required.sort_by_key(|capture| capture.field);
        for pair in required.windows(2) {
            if pair[0].owner == pair[1].owner
                && pair[0].field == pair[1].field
                && !pair[0].same_storage(pair[1])
            {
                return Err(self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape));
            }
        }
        required.dedup_by(|left, right| left.same_storage(*right));
        let mut identities = crate::fx_hash::HashSet::default();
        for binding in &required {
            if !binding
                .capture_identity
                .is_some_and(|identity| identities.insert(identity))
            {
                return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
            }
        }
        use crate::resolve::{ConstructorCapturePlan as Plan, ConstructorCaptureSource as Source};
        let operands = match plan {
            Plan::OwnStorage(owner) => {
                if *owner != classifier.classifier
                    || self.current_storage_owner() != Some(classifier.declaration)
                {
                    return Err(
                        self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    );
                }
                required
                    .iter()
                    .map(|required| {
                        let identity = required.capture_identity.ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        })?;
                        self.class_capture_binding(identity)
                            .map(ConstructorCaptureOperand::Class)
                            .ok_or_else(|| {
                                self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::MissingStableCallTarget,
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            Plan::Selected(projected) => {
                if projected.len() != required.len() {
                    return Err(
                        self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    );
                }
                required
                    .iter()
                    .map(|required| {
                        let identity = required.capture_identity.ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        })?;
                        let mut selected = projected
                            .iter()
                            .filter(|projection| projection.required == identity);
                        let projection = selected
                            .next()
                            .filter(|_| selected.next().is_none())
                            .ok_or_else(|| {
                                self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::MissingStableCallTarget,
                                )
                            })?;
                        match &projection.source {
                            Source::Captured => self
                                .class_capture_binding(identity)
                                .map(ConstructorCaptureOperand::Class)
                                .ok_or_else(|| {
                                    self.failure(
                                        Some(span),
                                        BodyCheckFailureKind::MissingStableCallTarget,
                                    )
                                }),
                            Source::ClassStorage(storage) => self
                                .selected_class_capture_binding(*storage, span)
                                .map(ConstructorCaptureOperand::Class),
                            Source::EnumEntryProperty { owner, ordinal } => {
                                if identity
                                    != (ClassCaptureIdentity::DeclaredProperty {
                                        owner: *owner,
                                        ordinal: *ordinal,
                                    })
                                {
                                    return Err(self.failure(
                                        Some(span),
                                        BodyCheckFailureKind::MissingStableCallTarget,
                                    ));
                                }
                                self.enum_entry_storage_binding(*owner, *ordinal, required.ty, span)
                                    .map(ConstructorCaptureOperand::Class)
                            }
                            Source::Receiver(receiver) => Ok(
                                ConstructorCaptureOperand::ImplicitReceiver(receiver.clone()),
                            ),
                            Source::Lexical {
                                name,
                                shadow_depth,
                                delegated: false,
                            } => {
                                let (depth, binding) = self
                                    .binding_source_at_shadow_depth(name, *shadow_depth as usize)
                                    .ok_or_else(|| {
                                        self.failure(
                                            Some(span),
                                            BodyCheckFailureKind::MissingStableCallTarget,
                                        )
                                    })?;
                                Ok(match depth {
                                    None => ConstructorCaptureOperand::Local(binding),
                                    Some(enclosing_depth) => ConstructorCaptureOperand::Captured {
                                        enclosing_depth,
                                        binding,
                                    },
                                })
                            }
                            Source::Lexical {
                                name,
                                shadow_depth,
                                delegated: true,
                            } => {
                                let (depth, delegate) = self
                                    .delegate_scopes
                                    .iter()
                                    .rev()
                                    .filter_map(|scope| {
                                        scope.get(name).cloned().map(|binding| (None, binding))
                                    })
                                    .chain(
                                        self.outer_delegates
                                            .get(name)
                                            .cloned()
                                            .map(|(depth, binding)| (Some(depth), binding)),
                                    )
                                    .nth(*shadow_depth as usize)
                                    .ok_or_else(|| {
                                        self.failure(
                                            Some(span),
                                            BodyCheckFailureKind::MissingStableCallTarget,
                                        )
                                    })?;
                                Ok(match delegate.storage {
                                    DelegateStorage::ClassField(binding) => {
                                        ConstructorCaptureOperand::Class(binding)
                                    }
                                    DelegateStorage::Local(binding) => match depth {
                                        None => ConstructorCaptureOperand::Local(binding),
                                        Some(enclosing_depth) => {
                                            ConstructorCaptureOperand::Captured {
                                                enclosing_depth,
                                                binding,
                                            }
                                        }
                                    },
                                })
                            }
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        if required.is_empty() {
            return Ok(None);
        }

        required
            .into_iter()
            .zip(operands)
            .map(|(required, operand)| {
                let available = match &operand {
                    ConstructorCaptureOperand::Class(binding) => *binding,
                    ConstructorCaptureOperand::Local(binding)
                    | ConstructorCaptureOperand::Captured { binding, .. } => ClassCaptureBinding {
                        owner: required.owner,
                        field: required.field,
                        ty: binding.ty,
                        shared_cell: required.shared_cell,
                        enclosing_depth: 0,
                        semantic_receiver_depth: None,
                        receiver_source: None,
                        enclosing_instance_owner: required.enclosing_instance_owner,
                        capture_identity: required.capture_identity,
                        lexical_binding: required.lexical_binding,
                    },
                    ConstructorCaptureOperand::ImplicitReceiver(_) => required,
                };
                if available.ty != required.ty
                    || available.shared_cell != required.shared_cell
                    || available.enclosing_instance_owner != required.enclosing_instance_owner
                {
                    return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                }
                let (kind, shared_cell_holder) = match operand {
                    ConstructorCaptureOperand::Class(binding) => (
                        self.class_storage_read_kind(
                            ClassCaptureBinding {
                                shared_cell: false,
                                ..binding
                            },
                            origin,
                        )?,
                        false,
                    ),
                    ConstructorCaptureOperand::Local(binding) => {
                        (FirExprKind::ValueRead(binding.value), required.shared_cell)
                    }
                    ConstructorCaptureOperand::Captured {
                        enclosing_depth,
                        binding,
                    } => {
                        self.body.add_capture(FirCapture {
                            origin,
                            enclosing_depth,
                            source: FirCaptureSource::Value(binding.value),
                            ty: binding.ty,
                            shared_cell: required.shared_cell,
                        });
                        (
                            FirExprKind::CapturedValueRead {
                                enclosing_depth,
                                source: binding.value,
                            },
                            required.shared_cell,
                        )
                    }
                    ConstructorCaptureOperand::ImplicitReceiver(selection) => {
                        if selection.ty != required.ty.get() {
                            return Err(self
                                .failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape));
                        }
                        let receiver = self
                            .materialize_implicit_receiver(origin, Some(span), &selection)?
                            .ok_or_else(|| {
                                self.failure(None, BodyCheckFailureKind::MissingStableCallTarget)
                            })?;
                        if receiver.conversion.is_some() {
                            return Err(
                                self.failure(None, BodyCheckFailureKind::UnsupportedCallShape)
                            );
                        }
                        return Ok(FirConstructorCaptureArgument {
                            value: receiver.value,
                            shared_cell_holder: false,
                        });
                    }
                };
                let value = self.body.add_expr(FirExpr {
                    origin,
                    ty: required.ty,
                    kind,
                });
                Ok(FirConstructorCaptureArgument {
                    value,
                    shared_cell_holder,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|arguments| Some(arguments.into_boxed_slice()))
    }

    /// Receiver captured into the current local classifier at this checked receiver-tower depth.
    /// Direct callable receivers occupy the prefix; captured class fields follow in the exact order
    /// published by `ClassBodyContext`. This is an identity coordinate, not a type search.
    pub(super) fn class_receiver_binding_at(
        &self,
        receiver_depth: u32,
    ) -> Option<ClassCaptureBinding> {
        if let Some(binding) = self
            .class_receivers
            .iter()
            .find(|binding| binding.semantic_receiver_depth == Some(receiver_depth))
        {
            return Some(*binding);
        }
        let ordinal = receiver_depth.checked_sub(self.owned_receiver_count)?;
        self.class_receivers.get(ordinal as usize).copied()
    }

    fn stable_classifier_for_parser(
        &self,
        transient: crate::ast::DeclId,
        span: Span,
    ) -> Option<DeclarationId> {
        match self.session.active_source.as_ref() {
            Some(active) => active.canonical_classifier_declaration(transient, self.index),
            None => self
                .index
                .declaration_at(self.source, span, DeclarationKind::Classifier),
        }
    }

    pub(super) fn nested_class_values(
        &self,
    ) -> Result<HashMap<String, ClassCaptureBinding>, BodyCheckFailure> {
        self.class_values
            .iter()
            .map(|(name, binding)| Ok((name.clone(), self.nested_class_binding(*binding)?)))
            .collect()
    }

    pub(super) fn nested_class_capture_values(
        &self,
    ) -> Result<Vec<(String, ClassCaptureBinding)>, BodyCheckFailure> {
        self.class_capture_values
            .iter()
            .map(|(name, binding)| Ok((name.clone(), self.nested_class_binding(*binding)?)))
            .collect()
    }

    /// Return the class field that carries one already-resolved closure value. Forwarded captures
    /// can share a source spelling while denoting different fields, so this lookup deliberately
    /// has no name-based recovery path.
    fn class_capture_binding(&self, identity: ClassCaptureIdentity) -> Option<ClassCaptureBinding> {
        let candidates = self
            .class_capture_values
            .iter()
            .map(|(_, binding)| *binding)
            .chain(self.class_receivers.iter().copied())
            .filter(|binding| binding.capture_identity == Some(identity));
        let depth = candidates
            .clone()
            .map(|binding| binding.enclosing_depth)
            .min()?;
        let mut nearest = candidates.filter(|binding| binding.enclosing_depth == depth);
        let selected = nearest.next()?;
        nearest
            .all(|binding| binding == selected)
            .then_some(selected)
    }

    /// The nearest class field that stores the binding whose lexical identity is `identity`.
    pub(super) fn class_value_by_lexical_binding(
        &self,
        identity: u32,
    ) -> Option<ClassCaptureBinding> {
        self.class_capture_values
            .iter()
            .map(|(_, binding)| *binding)
            .filter(|binding| binding.lexical_binding == Some(identity))
            .min_by_key(|binding| binding.enclosing_depth)
    }

    /// The local-class receiver fields a nested lambda or local function reads through this body.
    /// The nested body's own receivers (`nested_owned`: a receiver lambda's receiver, its context
    /// receivers) sit in front of this body's rungs in the resolver's receiver tower, so each
    /// field's semantic coordinate moves out by that many rungs. `build { … }` inside a local-class
    /// member names the local class's captured `this@outer` one rung further out than the member.
    pub(super) fn nested_class_receivers(
        &self,
        nested_owned: u32,
    ) -> Result<Vec<ClassCaptureBinding>, BodyCheckFailure> {
        self.class_receivers
            .iter()
            .copied()
            .map(|binding| {
                let mut binding = self.nested_class_binding(binding)?;
                binding.semantic_receiver_depth = binding
                    .semantic_receiver_depth
                    .map(|depth| {
                        depth.checked_add(nested_owned).ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::UnsupportedCallShape)
                        })
                    })
                    .transpose()?;
                Ok(binding)
            })
            .collect()
    }

    pub(super) fn nested_class_delegates(
        &self,
    ) -> Result<HashMap<String, LocalDelegateBinding>, BodyCheckFailure> {
        self.class_delegates
            .iter()
            .map(|(name, delegate)| {
                let mut delegate = delegate.clone();
                if let DelegateStorage::ClassField(binding) = delegate.storage {
                    delegate.storage =
                        DelegateStorage::ClassField(self.nested_class_binding(binding)?);
                }
                Ok((name.clone(), delegate))
            })
            .collect()
    }

    fn nested_class_binding(
        &self,
        mut binding: ClassCaptureBinding,
    ) -> Result<ClassCaptureBinding, BodyCheckFailure> {
        binding.receiver_source = Some(match binding.receiver_source {
            Some(mut source) => {
                source.enclosing_depth =
                    source.enclosing_depth.checked_add(1).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                source
            }
            None => {
                // The class dispatch receiver is the last receiver owned by a member body (after
                // extension and unnamed context receivers). Capture that exact tower coordinate;
                // an outer-class field is reached later through its checked structural path.
                let depth = self.owned_receiver_count.checked_sub(1).ok_or_else(|| {
                    self.failure(None, BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                ClassReceiverCaptureSource {
                    enclosing_depth: 0,
                    current: depth == 0,
                    depth,
                }
            }
        });
        Ok(binding)
    }

    /// Inner classifiers from `current` out to `owner`, not including `owner`.
    ///
    /// Empty means the field already lives on `current`. Each element is one enclosing-instance
    /// edge, the same shape an `EnclosingReceiver` path publishes.
    fn enclosing_capture_walk(
        &self,
        current: DeclarationId,
        owner: DeclarationId,
    ) -> Result<Box<[DeclarationId]>, BodyCheckFailure> {
        let mut classifier = current;
        let mut path = Vec::new();
        while classifier != owner {
            path.push(classifier);
            classifier = self
                .index
                .declaration_anchor(classifier)
                .and_then(|anchor| anchor.owner)
                .filter(|owner| {
                    self.index.declaration_anchor(*owner).is_some_and(|anchor| {
                        matches!(
                            anchor.kind,
                            crate::fir::DeclarationKind::Classifier
                                | crate::fir::DeclarationKind::EnumEntry
                        )
                    })
                })
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
        }
        Ok(path.into_boxed_slice())
    }

    fn instance_type_of(&self, declaration: DeclarationId) -> Result<ResolvedTy, BodyCheckFailure> {
        let semantic = self
            .index
            .classifier_header(declaration)
            .or_else(|| self.index.enclosing_classifier(declaration))
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
        ResolvedTy::new(Ty::obj_name(semantic.classifier))
            .map_err(|error| self.failure(None, BodyCheckFailureKind::UnpublishableType(error)))
    }

    fn captured_class_storage_receiver(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
    ) -> StorageReceiver {
        let Some(source) = binding.receiver_source else {
            return Ok(None);
        };
        let current = self
            .current_storage_owner()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
        let walk = self.enclosing_capture_walk(current, binding.owner)?;
        // The instance under construction does not exist in a constructor prefix, so an outer
        // capture cannot be read off `this`. The walk is the enclosing-instance path already
        // sitting in a constructor parameter; the lambda captures that owner instance and reads
        // the field from it. Capturing `this` and walking `this$0` instead feeds `invokedynamic`
        // an `uninitializedThis`.
        let prefix_outer = self.constructor_prefix_capture_access && !walk.is_empty();
        let receiver_owner = if prefix_outer { binding.owner } else { current };
        let receiver_ty = self.instance_type_of(receiver_owner)?;
        let capture_path: Box<[DeclarationId]> = if prefix_outer {
            walk.clone()
        } else {
            Box::new([])
        };
        let storage_path = if prefix_outer { Box::new([]) } else { walk };
        self.body
            .add_implicit_receiver_capture(FirImplicitReceiverCapture {
                origin,
                enclosing_depth: source.enclosing_depth,
                current: source.current,
                depth: source.depth,
                path: capture_path.clone(),
                ty: receiver_ty,
                receiver: self.enclosing_instance(receiver_owner),
            });
        let receiver = self.body.add_expr(FirExpr {
            origin,
            ty: receiver_ty,
            kind: FirExprKind::CapturedImplicitReceiver {
                enclosing_depth: source.enclosing_depth,
                current: source.current,
                depth: source.depth,
                path: capture_path,
            },
        });
        Ok(Some((receiver, storage_path)))
    }

    /// Read an outer class's capture from the constructor's enclosing-instance parameter.
    ///
    /// Used when the read is in the constructor body itself, so there is no lambda capture to
    /// hang the path on. A lambda takes the same walk through
    /// [`Self::captured_class_storage_receiver`].
    fn prefix_outer_storage_receiver(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
    ) -> StorageReceiver {
        if !self.constructor_prefix_capture_access || binding.enclosing_depth == 0 {
            return Ok(None);
        }
        let Some(current) = self.current_storage_owner() else {
            return Ok(None);
        };
        let walk = self.enclosing_capture_walk(current, binding.owner)?;
        if walk.is_empty() {
            return Ok(None);
        }
        let receiver = self.body.add_expr(FirExpr {
            origin,
            ty: self.instance_type_of(binding.owner)?,
            kind: FirExprKind::EnclosingReceiver { path: walk },
        });
        Ok(Some((receiver, Box::new([]))))
    }

    /// Exact dispatch receiver frame that owns `owner` and is visible to this local callable.
    /// Receiver-frame coordinates were fixed while entering each nested callable, so this does not
    /// re-run receiver selection.
    pub(super) fn enclosing_storage_receiver_source(
        &self,
        owner: DeclarationId,
    ) -> Option<ClassReceiverCaptureSource> {
        self.outer_receiver_frames
            .iter()
            .enumerate()
            .find_map(|(enclosing_depth, frame)| {
                (frame.dispatch_owner == Some(owner)).then(|| ClassReceiverCaptureSource {
                    enclosing_depth: u32::try_from(enclosing_depth)
                        .expect("too many nested receiver frames"),
                    current: frame.dispatch_depth == Some(0),
                    depth: frame
                        .dispatch_depth
                        .expect("a dispatch owner has a receiver depth"),
                })
            })
    }

    fn declared_class_storage_binding(
        &self,
        owner: DeclarationId,
        field: u32,
        ty: ResolvedTy,
    ) -> Result<ClassCaptureBinding, BodyCheckFailure> {
        let receiver_source = if self.body.local_callable().is_some() {
            Some(
                self.enclosing_storage_receiver_source(owner)
                    .ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingStableCallTarget)
                    })?,
            )
        } else {
            None
        };
        Ok(ClassCaptureBinding {
            owner,
            field,
            ty,
            shared_cell: false,
            enclosing_depth: 0,
            semantic_receiver_depth: None,
            receiver_source,
            enclosing_instance_owner: None,
            capture_identity: None,
            lexical_binding: None,
        })
    }

    /// How a read of the captured value `binding` stores reads it: from the constructor parameter
    /// when the resolver placed the read in its class's constructor code, else from storage.
    pub(super) fn capture_read_kind(
        &mut self,
        expression: crate::ast::ExprId,
        binding: ClassCaptureBinding,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if let Some(read) = self.construction_capture_read(expression, binding)? {
            return Ok(read);
        }
        let origin = self.expression_origin(expression)?;
        self.class_storage_read_kind(binding, origin)
    }

    /// Publish the exact enclosing receiver captured by a local callable that constructs a local
    /// classifier. Resolver receiver depths address the complete lexical tower; a plain lambda owns
    /// no receiver slots itself, so depth zero can still denote its enclosing class dispatch
    /// receiver. Treating `current && depth == 0` as the lambda's dispatch receiver loses that
    /// boundary. Reuse the same frame translation as ordinary implicit-receiver expressions and
    /// make the lifted receiver parameter explicit in both the body capture list and class capture.
    fn captured_callable_receiver_source(
        &mut self,
        origin: OriginId,
        receiver_depth: u32,
        ty: ResolvedTy,
    ) -> Result<Option<CheckedClassCaptureSource>, BodyCheckFailure> {
        if self.body.local_callable().is_none() || receiver_depth < self.owned_receiver_count {
            return Ok(None);
        }
        let (enclosing_depth, depth, path, receiver) = self
            .enclosing_receiver_capture(receiver_depth as usize)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        let current = depth == 0;
        let instance_owner = receiver.enclosing_owner();
        self.body
            .add_implicit_receiver_capture(FirImplicitReceiverCapture {
                origin,
                enclosing_depth,
                current,
                depth,
                path: path.clone(),
                ty,
                receiver,
            });
        Ok(Some(CheckedClassCaptureSource {
            instance_owner,
            source: FirLocalClassCaptureSource::CapturedImplicitReceiver {
                enclosing_depth,
                current,
                depth,
                path,
            },
        }))
    }

    /// Is a capture of `owner` being reached while the CONSTRUCTOR PREFIX is in scope — a
    /// superclass or sibling-constructor argument, or a constructor default?
    ///
    /// There the instance does not exist yet, so the capture has to come from the constructor
    /// parameter carrying it. Reading the storage instead loads it off a `this` the JVM verifier
    /// will not let you touch, and that a target without a verifier answers with a zero.
    fn reads_constructor_prefix_capture(&self, owner: DeclarationId, enclosing_depth: u32) -> bool {
        self.constructor_prefix_capture_access
            && enclosing_depth == 0
            && self
                .index
                .declaration_anchor(DeclarationId::from_raw(self.body.owner().raw()))
                .filter(|anchor| anchor.kind == DeclarationKind::Constructor)
                .and_then(|anchor| anchor.owner)
                == Some(owner)
    }

    /// A capture read the resolver placed in its class's constructor code, which reads the
    /// constructor parameter (`None` for any other read).
    ///
    /// Only a direct read in a body that runs as part of the owner's constructor can be one: a
    /// constructor body or default, a property initializer or delegate, or an `init` block. The
    /// resolver's walk and this body disagreeing about that is an internal error.
    pub(super) fn construction_capture_read(
        &self,
        expression: crate::ast::ExprId,
        binding: ClassCaptureBinding,
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        if !self
            .info
            .local_class_capture_layouts
            .construction_reads
            .contains(&expression)
        {
            return Ok(None);
        }
        let constructing = self.nested_body_depth == 0
            && binding.enclosing_depth == 0
            && self
                .index
                .declaration_anchor(DeclarationId::from_raw(self.body.owner().raw()))
                .filter(|anchor| {
                    matches!(
                        anchor.kind,
                        DeclarationKind::Constructor
                            | DeclarationKind::Initializer
                            | DeclarationKind::Property
                    )
                })
                .and_then(|anchor| anchor.owner)
                == Some(binding.owner);
        if !constructing {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            ));
        }
        Ok(Some(FirExprKind::ConstructorCaptureRead {
            owner: binding.owner,
            field: binding.field,
            shared_cell: binding.shared_cell,
            site: crate::fir::FirConstructorCaptureSite::Parameter,
        }))
    }

    /// Carry a constructor-prefix capture into the callable that reads it, and answer WHERE that
    /// callable finds it.
    ///
    /// In the constructor body itself the synthetic prefix parameter is already in scope and there
    /// is nothing to capture. A lambda nested in the prefix has no access to it, so it takes the
    /// value as one more capture parameter, bound at the construction site from that parameter.
    /// Only lambda frames can lie between the two: every other nested body clears the prefix rule.
    ///
    /// The answer is the read's exact coordinate and travels on the checked node. Lowering cannot
    /// work it out for itself: a capture registered at depth 0 and a read in the constructor body
    /// are both "depth 0", so a depth alone would not tell them apart, and discovering which by
    /// trying one lookup and reinterpreting the node on a miss is the fallback this exists to
    /// remove.
    fn capture_constructor_prefix(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
    ) -> crate::fir::FirConstructorCaptureSite {
        let Some(enclosing_depth) = self.nested_body_depth.checked_sub(1) else {
            return crate::fir::FirConstructorCaptureSite::Parameter;
        };
        self.body.add_capture(FirCapture {
            origin,
            enclosing_depth,
            source: FirCaptureSource::ConstructorPrefix {
                owner: binding.owner,
                field: binding.field,
            },
            ty: binding.ty,
            shared_cell: binding.shared_cell,
        });
        crate::fir::FirConstructorCaptureSite::Captured { enclosing_depth }
    }

    pub(super) fn class_storage_read_kind(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if self.reads_constructor_prefix_capture(binding.owner, binding.enclosing_depth) {
            let site = self.capture_constructor_prefix(binding, origin);
            return Ok(FirExprKind::ConstructorCaptureRead {
                owner: binding.owner,
                field: binding.field,
                shared_cell: binding.shared_cell,
                site,
            });
        }
        if let Some((receiver, path)) = self.captured_class_storage_receiver(binding, origin)? {
            return Ok(FirExprKind::CapturedClassStorageRead {
                owner: binding.owner,
                receiver,
                path,
                field: binding.field,
                shared_cell: binding.shared_cell,
            });
        }
        if let Some((receiver, path)) = self.prefix_outer_storage_receiver(binding, origin)? {
            return Ok(FirExprKind::CapturedClassStorageRead {
                owner: binding.owner,
                receiver,
                path,
                field: binding.field,
                shared_cell: binding.shared_cell,
            });
        }
        Ok(if binding.enclosing_depth != 0 {
            FirExprKind::EnclosingClassStorageRead {
                owner: binding.owner,
                enclosing_depth: binding.enclosing_depth,
                field: binding.field,
                shared_cell: binding.shared_cell,
            }
        } else if binding.shared_cell {
            FirExprKind::ClassStorageSharedRead {
                owner: binding.owner,
                field: binding.field,
            }
        } else {
            FirExprKind::ClassStorageRead {
                owner: binding.owner,
                field: binding.field,
            }
        })
    }

    pub(super) fn class_storage_shared_write_kind(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
        value: FirExprId,
        conversion: Option<FirConversion>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if self.reads_constructor_prefix_capture(binding.owner, binding.enclosing_depth) {
            let site = self.capture_constructor_prefix(binding, origin);
            return Ok(FirExprKind::ConstructorCaptureSharedWrite {
                owner: binding.owner,
                field: binding.field,
                element: binding.ty,
                value,
                conversion,
                site,
            });
        }
        if let Some((receiver, path)) = self.captured_class_storage_receiver(binding, origin)? {
            return Ok(FirExprKind::CapturedClassStorageSharedWrite {
                owner: binding.owner,
                receiver,
                path,
                field: binding.field,
                element: binding.ty,
                value,
                conversion,
            });
        }
        if let Some((receiver, path)) = self.prefix_outer_storage_receiver(binding, origin)? {
            return Ok(FirExprKind::CapturedClassStorageSharedWrite {
                owner: binding.owner,
                receiver,
                path,
                field: binding.field,
                element: binding.ty,
                value,
                conversion,
            });
        }
        Ok(FirExprKind::ClassStorageSharedWrite {
            owner: binding.owner,
            enclosing_depth: binding.enclosing_depth,
            field: binding.field,
            element: binding.ty,
            value,
            conversion,
        })
    }

    fn anonymous_receiver_capture_source(
        &mut self,
        origin: OriginId,
        semantic: AnonymousObjectReceiverSource,
        ty: ResolvedTy,
        allow_class_storage: bool,
    ) -> Result<CheckedClassCaptureSource, BodyCheckFailure> {
        let (current, depth, enclosing_instance) = match semantic {
            AnonymousObjectReceiverSource::EnclosingInstance { current, depth } => {
                (current, depth, true)
            }
            AnonymousObjectReceiverSource::ImplicitReceiver { current, depth } => {
                (current, depth, false)
            }
        };
        if let Some(binding) = allow_class_storage
            .then(|| self.class_receiver_binding_at(depth))
            .flatten()
            .filter(|binding| binding.ty == ty)
        {
            let source =
                if self.reads_constructor_prefix_capture(binding.owner, binding.enclosing_depth) {
                    FirLocalClassCaptureSource::ConstructorCapture {
                        owner: binding.owner,
                        field: binding.field,
                        site: self.capture_constructor_prefix(binding, origin),
                    }
                } else if let Some((receiver, path)) =
                    self.captured_class_storage_receiver(binding, origin)?
                {
                    FirLocalClassCaptureSource::CapturedClassStorage {
                        owner: binding.owner,
                        receiver,
                        path,
                        field: binding.field,
                    }
                } else {
                    FirLocalClassCaptureSource::ClassStorage {
                        owner: binding.owner,
                        enclosing_depth: binding.enclosing_depth,
                        field: binding.field,
                    }
                };
            return Ok(CheckedClassCaptureSource {
                source,
                instance_owner: binding.enclosing_instance_owner,
            });
        }
        if let Some(source) = self.captured_callable_receiver_source(origin, depth, ty)? {
            return Ok(source);
        }
        if enclosing_instance {
            if current && depth == 0 {
                return Ok(CheckedClassCaptureSource {
                    source: FirLocalClassCaptureSource::DispatchReceiver,
                    instance_owner: Some(self.path_instance_owner(&[], None)?),
                });
            }
            if let Some(path) =
                self.enclosing_receiver_path(&crate::resolve::ImplicitReceiverSelection {
                    ty: ty.get(),
                    current,
                    receiver_depth: depth as usize,
                    classifier: None,
                    context_binding: None,
                    singleton: None,
                    context_parameter: false,
                    construction_sites: Vec::new(),
                })
            {
                let instance_owner = Some(self.path_instance_owner(&path, None)?);
                return Ok(CheckedClassCaptureSource {
                    source: FirLocalClassCaptureSource::EnclosingReceiver { path },
                    instance_owner,
                });
            }
        }
        Ok(CheckedClassCaptureSource {
            source: FirLocalClassCaptureSource::ImplicitReceiver { current, depth },
            instance_owner: self
                .receiver_capture_at_depth(depth as usize)
                .and_then(|receiver| receiver.enclosing_owner()),
        })
    }

    fn checked_class_captures(
        &mut self,
        declaration: DeclarationId,
        span: Span,
        origin: OriginId,
        captures: &[crate::resolve::AnonymousObjectCapture],
    ) -> Result<Box<[FirLocalClassCapture]>, BodyCheckFailure> {
        let mut checked = Vec::with_capacity(captures.len());
        let capture_owner = self
            .index
            .classifier_header(declaration)
            .map(|classifier| classifier.classifier)
            .ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
        let mut context = ClassBodyContext {
            callables: self.nested_outer_callables(),
            enclosing_property: self.enclosing_property_id(),
            ..ClassBodyContext::default()
        };
        for (field, capture) in captures.iter().enumerate() {
            let field = u32::try_from(field).expect("too many local-class captures");
            let logical_identity = capture.logical_identity(capture_owner).ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
            // Transport the selected value identity independently of this declaration's final
            // constructor-prefix position, retaining the complete binding's physical role below.
            let capture_identity = Some(logical_identity);
            let mut ty = self.resolved_type(span, capture.ty)?;
            // A captured delegated property is its delegate's variable, named as such.
            let mut name = capture.name.clone();
            let mut instance_owner = None;
            let source = match capture.source {
                AnonymousObjectCaptureSource::LexicalValue => {
                    let delegate = self
                        .local_delegate(&capture.name)
                        .map(|binding| (u32::MAX, binding))
                        .or_else(|| self.outer_delegates.get(&capture.name).cloned());
                    if let Some((depth, mut delegate)) = delegate {
                        let storage = delegate.storage.local().ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::UnknownLocal)
                        })?;
                        ty = storage.ty;
                        name = delegate.storage_name.to_string();
                        let source = if depth == u32::MAX {
                            FirLocalClassCaptureSource::Value(storage.value)
                        } else {
                            self.body.add_capture(FirCapture {
                                origin,
                                enclosing_depth: depth,
                                source: FirCaptureSource::Value(storage.value),
                                ty: storage.ty,
                                shared_cell: false,
                            });
                            FirLocalClassCaptureSource::Captured {
                                enclosing_depth: depth,
                                source: storage.value,
                            }
                        };
                        delegate.storage = DelegateStorage::ClassField(ClassCaptureBinding {
                            owner: declaration,
                            field,
                            ty,
                            shared_cell: false,
                            enclosing_depth: 0,
                            semantic_receiver_depth: None,
                            receiver_source: None,
                            enclosing_instance_owner: None,
                            capture_identity,
                            lexical_binding: capture.lexical_binding,
                        });
                        if let DelegateStorage::ClassField(binding) = delegate.storage {
                            context.capture_values.push((capture.name.clone(), binding));
                        }
                        context.delegates.insert(capture.name.clone(), delegate);
                        source
                    } else if let Some((enclosing_depth, binding)) = self
                        .binding_source_at_shadow_depth(
                            &capture.name,
                            capture.lexical_shadow_depth as usize,
                        )
                    {
                        context.record_value(
                            capture.name.clone(),
                            ClassCaptureBinding {
                                owner: declaration,
                                field,
                                ty,
                                shared_cell: capture.shared_cell,
                                enclosing_depth: 0,
                                semantic_receiver_depth: None,
                                receiver_source: None,
                                enclosing_instance_owner: None,
                                capture_identity,
                                lexical_binding: capture.lexical_binding,
                            },
                            capture.lexical_shadow_depth,
                        );
                        if let Some(enclosing_depth) = enclosing_depth {
                            self.body.add_capture(FirCapture {
                                origin,
                                enclosing_depth,
                                source: FirCaptureSource::Value(binding.value),
                                ty: binding.ty,
                                shared_cell: false,
                            });
                            FirLocalClassCaptureSource::Captured {
                                enclosing_depth,
                                source: binding.value,
                            }
                        } else {
                            FirLocalClassCaptureSource::Value(binding.value)
                        }
                    } else if let Some(identity) = capture.capture_dependency {
                        let binding = self.class_capture_binding(identity).ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        })?;
                        context.record_value(
                            capture.name.clone(),
                            ClassCaptureBinding {
                                owner: declaration,
                                field,
                                ty,
                                shared_cell: capture.shared_cell,
                                enclosing_depth: 0,
                                semantic_receiver_depth: None,
                                receiver_source: None,
                                enclosing_instance_owner: None,
                                capture_identity,
                                lexical_binding: capture.lexical_binding,
                            },
                            capture.lexical_shadow_depth,
                        );
                        if self.reads_constructor_prefix_capture(
                            binding.owner,
                            binding.enclosing_depth,
                        ) {
                            FirLocalClassCaptureSource::ConstructorCapture {
                                owner: binding.owner,
                                field: binding.field,
                                site: self.capture_constructor_prefix(binding, origin),
                            }
                        } else if let Some((receiver, path)) =
                            self.captured_class_storage_receiver(binding, origin)?
                        {
                            FirLocalClassCaptureSource::CapturedClassStorage {
                                owner: binding.owner,
                                receiver,
                                path,
                                field: binding.field,
                            }
                        } else {
                            FirLocalClassCaptureSource::ClassStorage {
                                owner: binding.owner,
                                enclosing_depth: binding.enclosing_depth,
                                field: binding.field,
                            }
                        }
                    } else {
                        return Err(self.failure(Some(span), BodyCheckFailureKind::UnknownLocal));
                    }
                }
                source @ (AnonymousObjectCaptureSource::ClassStorage(_)
                | AnonymousObjectCaptureSource::EnumEntryPropertyStorage { .. }) => {
                    let (source_binding, declared_property) = match source {
                        AnonymousObjectCaptureSource::ClassStorage(storage) => (
                            Some(self.selected_class_capture_binding(storage, span)?),
                            None,
                        ),
                        AnonymousObjectCaptureSource::EnumEntryPropertyStorage {
                            owner: expected,
                            field,
                        } => {
                            let owner = self.selected_enum_entry_storage_owner(
                                expected,
                                field,
                                Some(span),
                            )?;
                            (
                                self.selected_storage_capture_binding(owner, field, span)?,
                                Some((owner, field)),
                            )
                        }
                        _ => unreachable!("matched storage source"),
                    };
                    instance_owner =
                        source_binding.and_then(|binding| binding.enclosing_instance_owner);
                    let hoisted_receiver = self
                        .hoist_anonymous_super_argument
                        .then_some(capture.semantic_receiver)
                        .flatten();
                    if hoisted_receiver.is_none()
                        && matches!(
                            source,
                            AnonymousObjectCaptureSource::EnumEntryPropertyStorage { .. }
                        )
                    {
                        if let Some(identity) = capture.capture_dependency {
                            let Some(binding) = source_binding else {
                                return Err(self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::MissingStableCallTarget,
                                ));
                            };
                            if binding.capture_identity != Some(identity) {
                                return Err(self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::MissingStableCallTarget,
                                ));
                            }
                        }
                    }
                    let semantic_receiver_depth = capture
                        .semantic_receiver
                        .map(AnonymousObjectReceiverSource::depth)
                        .map(|depth| {
                            depth.checked_add(1).ok_or_else(|| {
                                self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                            })
                        })
                        .transpose()?;
                    let captured_binding = ClassCaptureBinding {
                        owner: declaration,
                        field,
                        ty,
                        shared_cell: capture.shared_cell,
                        enclosing_depth: 0,
                        semantic_receiver_depth,
                        receiver_source: None,
                        enclosing_instance_owner: None,
                        capture_identity,
                        // A field forwarded from the enclosing class holds that field's binding.
                        lexical_binding: capture
                            .lexical_binding
                            .or(source_binding.and_then(|binding| binding.lexical_binding)),
                    };
                    if semantic_receiver_depth.is_some() {
                        context.receivers.push(captured_binding);
                    } else {
                        context.record_value(
                            capture.name.clone(),
                            captured_binding,
                            capture.lexical_shadow_depth,
                        );
                    }
                    if let Some(semantic) = hoisted_receiver {
                        let selected =
                            self.anonymous_receiver_capture_source(origin, semantic, ty, false)?;
                        instance_owner = selected.instance_owner;
                        selected.source
                    } else if let Some(binding) = source_binding {
                        if self.reads_constructor_prefix_capture(
                            binding.owner,
                            binding.enclosing_depth,
                        ) {
                            FirLocalClassCaptureSource::ConstructorCapture {
                                owner: binding.owner,
                                field: binding.field,
                                site: self.capture_constructor_prefix(binding, origin),
                            }
                        } else if let Some((receiver, path)) =
                            self.captured_class_storage_receiver(binding, origin)?
                        {
                            // The selected logical value's realized binding owns both the receiver
                            // walk and its finalized constructor-capture coordinate.
                            FirLocalClassCaptureSource::CapturedClassStorage {
                                owner: binding.owner,
                                receiver,
                                path,
                                field: binding.field,
                            }
                        } else {
                            FirLocalClassCaptureSource::ClassStorage {
                                owner: binding.owner,
                                enclosing_depth: binding.enclosing_depth,
                                field: binding.field,
                            }
                        }
                    } else {
                        let (source_owner, source_field) = declared_property
                            .expect("only declared property storage can have no capture binding");
                        if self.reads_constructor_prefix_capture(source_owner, 0) {
                            FirLocalClassCaptureSource::ConstructorCapture {
                                owner: source_owner,
                                field: source_field,
                                site: self.capture_constructor_prefix(
                                    ClassCaptureBinding {
                                        owner: source_owner,
                                        field: source_field,
                                        ty,
                                        shared_cell: capture.shared_cell,
                                        enclosing_depth: 0,
                                        semantic_receiver_depth: None,
                                        receiver_source: None,
                                        enclosing_instance_owner: None,
                                        capture_identity: None,
                                        lexical_binding: None,
                                    },
                                    origin,
                                ),
                            }
                        } else {
                            FirLocalClassCaptureSource::ClassStorage {
                                owner: source_owner,
                                enclosing_depth: 0,
                                field: source_field,
                            }
                        }
                    }
                }
                AnonymousObjectCaptureSource::EnclosingInstance { current, depth } => {
                    let semantic_receiver_depth = capture
                        .semantic_receiver
                        .map(AnonymousObjectReceiverSource::depth)
                        .unwrap_or(depth)
                        .checked_add(1)
                        .ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                    context.receivers.push(ClassCaptureBinding {
                        owner: declaration,
                        field,
                        ty,
                        shared_cell: false,
                        enclosing_depth: 0,
                        semantic_receiver_depth: Some(semantic_receiver_depth),
                        receiver_source: None,
                        enclosing_instance_owner: None,
                        capture_identity,
                        lexical_binding: None,
                    });
                    let selected = self.anonymous_receiver_capture_source(
                        origin,
                        AnonymousObjectReceiverSource::EnclosingInstance { current, depth },
                        ty,
                        true,
                    )?;
                    instance_owner = selected.instance_owner;
                    selected.source
                }
                AnonymousObjectCaptureSource::ImplicitReceiver { current, depth } => {
                    let semantic_receiver_depth = capture
                        .semantic_receiver
                        .map(AnonymousObjectReceiverSource::depth)
                        .unwrap_or(depth)
                        .checked_add(1)
                        .ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                        })?;
                    context.receivers.push(ClassCaptureBinding {
                        owner: declaration,
                        field,
                        ty,
                        shared_cell: false,
                        enclosing_depth: 0,
                        semantic_receiver_depth: Some(semantic_receiver_depth),
                        receiver_source: None,
                        enclosing_instance_owner: None,
                        capture_identity,
                        lexical_binding: None,
                    });
                    let selected = self.anonymous_receiver_capture_source(
                        origin,
                        AnonymousObjectReceiverSource::ImplicitReceiver { current, depth },
                        ty,
                        true,
                    )?;
                    instance_owner = selected.instance_owner;
                    selected.source
                }
            };
            let receiver =
                self.checked_capture_receiver(capture.receiver.as_ref(), instance_owner, span)?;
            for binding in context
                .receivers
                .iter_mut()
                .chain(
                    context
                        .capture_values
                        .iter_mut()
                        .map(|(_, binding)| binding),
                )
                .chain(context.values.values_mut())
                .filter(|binding| binding.owner == declaration && binding.field == field)
            {
                binding.enclosing_instance_owner = instance_owner;
            }
            // A resolved forward and each constructor demand retain lookup views of this same
            // physical operand. Never change the canonical capture or introduce another ABI slot.
            let mut views = capture.constructor_projections.clone();
            if let Some(source) = capture.source_identity(capture_owner) {
                if !views.contains(&source) {
                    views.push(source);
                }
            }
            if views.iter().any(|view| Some(*view) != capture_identity) {
                let binding = context
                    .capture_values
                    .iter()
                    .map(|(_, binding)| *binding)
                    .chain(context.receivers.iter().copied())
                    .find(|binding| binding.owner == declaration && binding.field == field)
                    .ok_or_else(|| {
                        self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    })?;
                for view in views
                    .into_iter()
                    .filter(|view| Some(*view) != capture_identity)
                {
                    context.capture_values.push((
                        capture.name.clone(),
                        ClassCaptureBinding {
                            capture_identity: Some(view),
                            ..binding
                        },
                    ));
                }
            }
            checked.push(FirLocalClassCapture {
                origin,
                name: name.into_boxed_str(),
                ty,
                shared_cell: capture.shared_cell,
                capture_identity,
                source,
                receiver,
                stored: true,
            });
        }
        // Retain a positive checked-empty environment too: absence means unavailable, not zero.
        crate::trace_compiler!(
            "fir",
            "record class capture context declaration={declaration:?} values={} delegates={} callables={} receivers={}",
            context.values.len(),
            context.delegates.len(),
            context.callables.len(),
            context.receivers.len(),
        );
        // This is part of the checked FIR fragment's closure semantics. If the surrounding
        // body is inline or a default expression, it survives only as payload of that retained
        // body. The session copy serves sibling body callbacks in the current pass and is
        // always transient.
        self.body
            .record_class_body_context(declaration, context.clone());
        self.session
            .class_bodies
            .entry(declaration)
            .or_default()
            .merge(context);
        Ok(checked.into_boxed_slice())
    }

    pub(super) fn anonymous_object(
        &mut self,
        expression: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let transient = self
            .file
            .anonymous_object_classes
            .get(&expression)
            .copied()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?;
        let crate::ast::Decl::Class(class) = self.file.decl(transient) else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            ));
        };
        let span = class.span;
        let declaration = self
            .stable_classifier_for_parser(transient, span)
            .ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
        let origin = self.expression_origin(expression)?;
        let captures = self
            .info
            .anonymous_object_captures_by_construction
            .get(&expression)
            .cloned()
            .unwrap_or_default();
        crate::trace_compiler!(
            "fir",
            "anonymous body captures declaration={declaration:?} expression={expression:?} captures={captures:?}",
        );
        let mut captures = self.checked_class_captures(declaration, span, origin, &captures)?;
        if !captures.is_empty() {
            let stored = self
                .info
                .local_class_capture_layouts
                .stored
                .get(&transient)
                .filter(|stored| stored.len() == captures.len())
                .ok_or_else(|| {
                    self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                })?;
            for (capture, stored) in captures.iter_mut().zip(stored.iter().copied()) {
                capture.stored = stored;
            }
        }
        crate::trace_compiler!(
            "fir",
            "anonymous checked captures declaration={declaration:?} captures={}",
            captures.len(),
        );
        let plan = self.index.classifier_header(declaration).ok_or_else(|| {
            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
        })?;
        crate::trace_compiler!(
            "fir",
            "anonymous delegate FIR declaration={declaration:?} ast={} resolved={} captures={} sources={:?}",
            class.interface_delegations.len(),
            plan.interface_delegations.len(),
            captures.len(),
            plan.interface_delegations
                .iter()
                .map(|delegation| delegation.source)
                .collect::<Vec<_>>(),
        );
        if plan.interface_delegations.len() != class.interface_delegations.len() {
            return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
        }
        let mut delegate_arguments = Vec::new();
        for (ordinal, (delegation, resolved)) in class
            .interface_delegations
            .iter()
            .zip(plan.interface_delegations.iter())
            .enumerate()
        {
            let crate::fir::ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(
                parameter,
            ) = resolved.source
            else {
                return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
            };
            let expected_parameter = captures
                .len()
                .checked_add(delegate_arguments.len())
                .and_then(|parameter| u32::try_from(parameter).ok())
                .ok_or_else(|| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?;
            if parameter != expected_parameter {
                return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
            }
            delegate_arguments.push(FirInterfaceDelegateArgument {
                delegation: u32::try_from(ordinal).map_err(|_| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?,
                value: self.value_at_selected_boundary(delegation.value, resolved.interface)?,
            });
        }
        let super_arguments = self.anonymous_super_arguments(transient, class)?;
        Ok(FirExprKind::AnonymousObject(FirAnonymousObject {
            declaration,
            captures,
            delegate_arguments: delegate_arguments.into_boxed_slice(),
            super_arguments,
        }))
    }

    /// Non-constant, non-capture super-constructor arguments of an anonymous object are evaluated
    /// at the construction site. Constants and lexical captures stay in the anonymous constructor,
    /// and a cast or not-null assertion of either stays with them. A cast of anything else stays
    /// in the constructor around the forwarded operand.
    fn anonymous_super_arguments(
        &mut self,
        transient: DeclId,
        class: &crate::ast::ClassDecl,
    ) -> Result<Box<[FirAnonymousSuperArgument]>, BodyCheckFailure> {
        let delegation = self
            .info
            .resolved_primary_ctor_delegations
            .get(&transient)
            .map(|resolved| {
                (
                    resolved.argument_slots.clone(),
                    resolved.argument_types.clone(),
                    resolved.vararg,
                )
            });
        let forwarded = self
            .info
            .anonymous_forwarded_super_arguments
            .get(&transient)
            .cloned()
            .unwrap_or_default();
        if forwarded.is_empty() {
            return Ok(Box::new([]));
        }
        let (argument_slots, argument_types, vararg) = delegation.ok_or_else(|| {
            self.failure(
                Some(class.span),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        let mut forwards = Vec::new();
        for (index, argument) in class.base_args.iter().copied().enumerate() {
            if !forwarded.contains(&argument) {
                continue;
            }
            let (core, shells) = peel_super_type_operators(self.file, argument);
            let slot = argument_slots.get(index).copied().ok_or_else(|| {
                self.failure(Some(class.span), BodyCheckFailureKind::UnsupportedCallShape)
            })?;
            if vararg == Some(slot) {
                continue;
            }
            let slot = u32::try_from(slot).map_err(|_| {
                self.failure(Some(class.span), BodyCheckFailureKind::UnsupportedCallShape)
            })?;
            let previous = self.hoist_anonymous_super_argument;
            self.hoist_anonymous_super_argument = true;
            self.construction_site_receiver_frames.push(transient);
            let value = (|| {
                if shells == 0 {
                    let expected = argument_types.get(index).copied().ok_or_else(|| {
                        self.failure(Some(class.span), BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                    let target = crate::fir::ResolvedTy::new(expected).map_err(|error| {
                        self.failure(
                            self.file.expr_span(argument),
                            BodyCheckFailureKind::UnpublishableType(error),
                        )
                    })?;
                    self.value_at_selected_boundary(argument, target)
                } else {
                    self.expression(core)
                }
            })();
            self.construction_site_receiver_frames
                .pop()
                .expect("anonymous super argument frame");
            self.hoist_anonymous_super_argument = previous;
            let value = value?;
            forwards.push(FirAnonymousSuperArgument {
                slot,
                value,
                type_operator_shells: shells,
            });
        }
        Ok(forwards.into_boxed_slice())
    }

    pub(super) fn local_class_enclosing_receiver(
        &mut self,
        origin: OriginId,
        selected: &crate::resolve::ImplicitReceiverSelection,
    ) -> Result<Option<FirReceiver>, BodyCheckFailure> {
        let selected_ty = ResolvedTy::new(selected.ty)
            .map_err(|error| self.failure(None, BodyCheckFailureKind::UnpublishableType(error)))?;
        let selected_depth = u32::try_from(selected.receiver_depth)
            .map_err(|_| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        let stable_classifier_binding = selected.classifier.and_then(|declaration| {
            let classifier = self.index.classifier_header(declaration)?.classifier;
            self.class_receivers.iter().copied().find(|binding| {
                binding.ty.get().non_null().kotlin_class_internal() == Some(classifier)
            })
        });
        // `selected_depth` is the resolver-published identity of this receiver-tower rung.  The
        // capture field can retain an enclosing generic spelling while postponed call inference
        // has already specialized the selected receiver (for example `Buildee<T>` captured by an
        // anonymous object inside a `build { ... }` call whose `T` is `String`).  Requiring type
        // equality here discards that exact identity and incorrectly republishes the receiver as a
        // direct callable slot.  Read the selected capture field and expose its checked, specialized
        // semantic type; lowering then only consumes the explicit storage identity.
        // A resolved class receiver has a stable classifier identity. If this local class has no
        // capture field for that classifier, it is reached through the checked `inner` ownership
        // path below. Falling back to the numeric receiver coordinate can alias an unrelated,
        // farther captured class receiver (for example selecting local `B` inside `B.C` but reading
        // `B`'s captured outer `A` field). Non-class receivers have no classifier identity and keep
        // their exact semantic coordinate path.
        let captured_binding = if selected.classifier.is_some() {
            stable_classifier_binding
        } else {
            self.class_receiver_binding_at(selected_depth)
        };
        if let Some(binding) = captured_binding {
            let kind = self.class_storage_read_kind(binding, origin)?;
            let value = self.body.add_expr(FirExpr {
                origin,
                ty: selected_ty,
                kind,
            });
            return Ok(Some(FirReceiver {
                value,
                conversion: None,
            }));
        }
        let owner = DeclarationId::from_raw(self.body.owner().raw());
        let Some(classifier) = self.index.enclosing_classifier(owner) else {
            return Ok(None);
        };
        let Some(header) = self.index.declaration_header(classifier.declaration) else {
            return Ok(None);
        };
        if !header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS) {
            return Ok(None);
        }
        let transient = match self.session.active_source.as_ref() {
            Some(active) => active
                .class(self.file, classifier.declaration)
                .map(|(declaration, _)| declaration),
            None => {
                let Some(range) = self.index.declaration_range(classifier.declaration) else {
                    return Ok(None);
                };
                self.file
                    .local_class_decls
                    .values()
                    .copied()
                    .find(|declaration| {
                        matches!(
                            self.file.decl(*declaration),
                            crate::ast::Decl::Class(class) if class.span == range
                        )
                    })
            }
        };
        let captures_by_class = if header
            .flags
            .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
        {
            &self.info.anonymous_object_captures_by_class
        } else {
            &self.info.local_class_captures_by_class
        };
        let Some(captures) = transient.and_then(|declaration| captures_by_class.get(&declaration))
        else {
            return Ok(None);
        };
        // A selected class receiver is addressed by the resolved classifier retained on the
        // checked capture. Its old receiver-tower depth is not an identity: another same-depth
        // receiver can become visible when this constructor body is checked in isolation. A
        // non-class receiver has no classifier identity and retains its exact semantic coordinate.
        let selected_classifier = match selected.classifier {
            Some(declaration) => {
                let Some(header) = self.index.classifier_header(declaration) else {
                    return Ok(None);
                };
                Some(header.classifier)
            }
            None => None,
        };
        let source_depth = selected_depth.checked_sub(self.owned_receiver_count);
        let Some((field, capture)) = captures.iter().enumerate().find(|(_, capture)| {
            if let Some(selected_classifier) = selected_classifier {
                return matches!(
                    capture.receiver.as_ref(),
                    Some(FirCapturedReceiver::Enclosing { classifier, .. })
                        if *classifier == selected_classifier
                );
            }
            let Some(source_depth) = source_depth else {
                return false;
            };
            match capture.source {
                AnonymousObjectCaptureSource::ImplicitReceiver { depth, .. }
                | AnonymousObjectCaptureSource::EnclosingInstance { depth, .. } => {
                    depth == source_depth
                }
                AnonymousObjectCaptureSource::LexicalValue
                | AnonymousObjectCaptureSource::ClassStorage(_)
                | AnonymousObjectCaptureSource::EnumEntryPropertyStorage { .. } => false,
            }
        }) else {
            return Ok(None);
        };
        if capture.ty.canonical_semantic() != selected_ty.get() {
            return Err(self.failure(None, BodyCheckFailureKind::MissingStableCallTarget));
        }
        let binding = ClassCaptureBinding {
            owner: classifier.declaration,
            field: u32::try_from(field).expect("too many local-class captures"),
            ty: selected_ty,
            shared_cell: false,
            enclosing_depth: 0,
            semantic_receiver_depth: Some(selected_depth),
            receiver_source: None,
            enclosing_instance_owner: None,
            capture_identity: None,
            lexical_binding: None,
        };
        let kind = self.class_storage_read_kind(binding, origin)?;
        let value = self.body.add_expr(FirExpr {
            origin,
            ty: selected_ty,
            kind,
        });
        Ok(Some(FirReceiver {
            value,
            conversion: None,
        }))
    }

    pub(super) fn local_class_statement(
        &mut self,
        statement: StmtId,
        class: &crate::ast::ClassDecl,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let transient = self
            .file
            .local_class_decls
            .get(&statement)
            .copied()
            .ok_or_else(|| {
                self.failure(
                    Some(class.span),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?;
        let declaration = self
            .stable_classifier_for_parser(transient, class.span)
            .ok_or_else(|| {
                self.failure(
                    Some(class.span),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?;
        let captures = self
            .info
            .local_class_captures_by_class
            .get(&transient)
            .cloned()
            .unwrap_or_default();
        crate::trace_compiler!(
            "fir",
            "local class body captures declaration={declaration:?} statement={statement:?} captures={captures:?}",
        );
        // A local class records the first-use order and the storage its checked body published.
        let layouts = &self.info.local_class_capture_layouts;
        let (capture_order, stored) = match (
            layouts.orders.get(&transient),
            layouts.stored.get(&transient),
        ) {
            (Some(order), Some(stored))
                if order.len() == captures.len() && stored.len() == captures.len() =>
            {
                (order.clone(), stored.clone())
            }
            (None, None) if captures.is_empty() => (Box::default(), Box::default()),
            _ => {
                return Err(self.failure(
                    Some(class.span),
                    BodyCheckFailureKind::MissingStableCallTarget,
                ))
            }
        };
        let mut captures =
            self.checked_class_captures(declaration, class.span, origin, &captures)?;
        for (capture, stored) in captures.iter_mut().zip(stored) {
            capture.stored = stored;
        }
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::LocalDeclaration {
                capture_order,
                declaration,
                captures,
            },
        }))
    }
}

pub(super) fn peel_super_type_operators(
    file: &crate::ast::File,
    mut expression: crate::ast::ExprId,
) -> (crate::ast::ExprId, u8) {
    let mut shells = 0u8;
    loop {
        match file.expr(expression) {
            crate::ast::Expr::As { operand, .. } | crate::ast::Expr::NotNull { operand } => {
                expression = *operand;
                shells = shells.saturating_add(1);
            }
            _ => return (expression, shells),
        }
    }
}
