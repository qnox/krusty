//! Checked local-class declaration and lexical-capture identities.

use super::*;
use crate::resolve::AnonymousObjectCaptureSource;

#[derive(Clone, Copy)]
enum ConstructorCaptureOperand {
    Class(ClassCaptureBinding),
    Local(LocalBinding),
    Captured {
        enclosing_depth: u32,
        binding: LocalBinding,
    },
    ImplicitReceiver {
        receiver_depth: u32,
    },
}

enum RequiredConstructorCapture {
    Value(String, ClassCaptureBinding),
    Delegate(String, ClassCaptureBinding),
    Receiver(ClassCaptureBinding),
}

impl RequiredConstructorCapture {
    fn binding(&self) -> ClassCaptureBinding {
        match self {
            Self::Value(_, binding) | Self::Delegate(_, binding) | Self::Receiver(binding) => {
                *binding
            }
        }
    }
}

impl BodyFirChecker<'_> {
    /// Publish the capture prefix for a local-class construction that crosses a streamed body
    /// boundary. Inside the declaring body the checked `LocalDeclaration` already owns the
    /// capture sources. A member of that class (or a nested class) instead sees the same captures
    /// through stable `(owner, field)` identities and must carry those reads on the call itself.
    pub(super) fn external_constructor_capture_arguments(
        &mut self,
        constructor: DeclarationId,
        origin: OriginId,
    ) -> Result<Option<Box<[FirConstructorCaptureArgument]>>, BodyCheckFailure> {
        let Some(classifier) = self.index.enclosing_classifier(constructor) else {
            return Ok(None);
        };
        if self.body.owns_class_body_context(classifier.declaration) {
            return Ok(None);
        }
        let Some(context) = self
            .session
            .class_bodies
            .get(&classifier.declaration)
            .cloned()
        else {
            return Ok(None);
        };
        let value_captures = if context.capture_values.is_empty() {
            context.values.into_iter().collect::<Vec<_>>()
        } else {
            context.capture_values
        };
        let mut required = value_captures
            .into_iter()
            .map(|(name, binding)| RequiredConstructorCapture::Value(name, binding))
            .chain(
                context
                    .delegates
                    .into_iter()
                    .filter_map(|(name, delegate)| {
                        let DelegateStorage::ClassField(binding) = delegate.storage else {
                            return None;
                        };
                        Some(RequiredConstructorCapture::Delegate(name, binding))
                    }),
            )
            .chain(
                context
                    .receivers
                    .into_iter()
                    .map(RequiredConstructorCapture::Receiver),
            )
            .collect::<Vec<_>>();
        required.sort_unstable_by_key(|capture| capture.binding().field);
        for pair in required.windows(2) {
            let left = pair[0].binding();
            let right = pair[1].binding();
            if left.owner == right.owner
                && left.field == right.field
                && (left.ty != right.ty || left.shared_cell != right.shared_cell)
            {
                return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
            }
        }
        required.dedup_by_key(|capture| {
            let binding = capture.binding();
            (binding.owner, binding.field)
        });
        if required.is_empty() {
            return Ok(None);
        }

        let operands = required
            .iter()
            .map(|required| {
                let binding = required.binding();
                let class = self
                    .class_capture_values
                    .iter()
                    .map(|(_, binding)| *binding)
                    .chain(self.class_values.values().copied())
                    .chain(self.class_delegates.values().filter_map(|delegate| {
                        if let DelegateStorage::ClassField(binding) = delegate.storage {
                            Some(binding)
                        } else {
                            None
                        }
                    }))
                    .chain(self.class_receivers.iter().copied())
                    .find(|available| {
                        (available.owner == binding.owner && available.field == binding.field)
                            || binding.capture_identity.is_some()
                                && available.capture_identity == binding.capture_identity
                    })
                    .or_else(|| match required {
                        RequiredConstructorCapture::Value(name, _) => {
                            self.class_values.get(name).copied()
                        }
                        RequiredConstructorCapture::Delegate(name, _) => self
                            .class_delegates
                            .get(name)
                            .and_then(|delegate| match delegate.storage {
                                DelegateStorage::ClassField(binding) => Some(binding),
                                DelegateStorage::Local(_) => None,
                            }),
                        RequiredConstructorCapture::Receiver(required) => {
                            self.class_receivers.iter().copied().find(|available| {
                                available.semantic_receiver_depth
                                    == required.semantic_receiver_depth
                                    && available.ty == required.ty
                            })
                        }
                    })
                    .map(ConstructorCaptureOperand::Class);
                class.or_else(|| match required {
                    RequiredConstructorCapture::Value(name, _) => self
                        .local_binding(name)
                        .map(ConstructorCaptureOperand::Local)
                        .or_else(|| {
                            self.outer_values.get(name).copied().map(
                                |(enclosing_depth, binding)| ConstructorCaptureOperand::Captured {
                                    enclosing_depth,
                                    binding,
                                },
                            )
                        }),
                    RequiredConstructorCapture::Delegate(name, _) => self
                        .local_delegate(name)
                        .and_then(|delegate| delegate.storage.local())
                        .map(ConstructorCaptureOperand::Local)
                        .or_else(|| {
                            self.outer_delegates.get(name).and_then(
                                |(enclosing_depth, delegate)| {
                                    delegate.storage.local().map(|binding| {
                                        ConstructorCaptureOperand::Captured {
                                            enclosing_depth: *enclosing_depth,
                                            binding,
                                        }
                                    })
                                },
                            )
                        }),
                    RequiredConstructorCapture::Receiver(binding) => binding
                        .semantic_receiver_depth
                        .and_then(|depth| depth.checked_sub(1))
                        .map(
                            |receiver_depth| ConstructorCaptureOperand::ImplicitReceiver {
                                receiver_depth,
                            },
                        ),
                })
            })
            .collect::<Vec<_>>();
        if operands.iter().any(Option::is_none) {
            return Err(self.failure(None, BodyCheckFailureKind::MissingStableCallTarget));
        }

        required
            .into_iter()
            .zip(operands)
            .map(|(required, operand)| {
                let required = required.binding();
                let operand = operand.expect("complete checked capture operand set");
                let available = match operand {
                    ConstructorCaptureOperand::Class(binding) => binding,
                    ConstructorCaptureOperand::Local(binding)
                    | ConstructorCaptureOperand::Captured { binding, .. } => ClassCaptureBinding {
                        owner: required.owner,
                        field: required.field,
                        ty: binding.ty,
                        shared_cell: required.shared_cell,
                        enclosing_depth: 0,
                        semantic_receiver_depth: None,
                        receiver_source: None,
                        capture_identity: required.capture_identity,
                    },
                    ConstructorCaptureOperand::ImplicitReceiver { .. } => required,
                };
                if available.ty != required.ty || available.shared_cell != required.shared_cell {
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
                            source: binding.value,
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
                    ConstructorCaptureOperand::ImplicitReceiver { receiver_depth } => {
                        let receiver = self
                            .materialize_implicit_receiver(
                                origin,
                                None,
                                &crate::resolve::ImplicitReceiverSelection {
                                    ty: required.ty.get(),
                                    current: receiver_depth == 0,
                                    receiver_depth: receiver_depth as usize,
                                    classifier: None,
                                    context_binding: None,
                                    singleton: None,
                                },
                            )?
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

    pub(super) fn nested_class_receivers(
        &self,
    ) -> Result<Vec<ClassCaptureBinding>, BodyCheckFailure> {
        self.class_receivers
            .iter()
            .copied()
            .map(|binding| self.nested_class_binding(binding))
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

    fn captured_class_storage_receiver(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
    ) -> Result<Option<(FirExprId, Box<[DeclarationId]>)>, BodyCheckFailure> {
        let Some(source) = binding.receiver_source else {
            return Ok(None);
        };
        let current = self
            .current_storage_owner()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
        let semantic_classifier = self
            .index
            .classifier_header(current)
            .or_else(|| self.index.enclosing_classifier(current))
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
        let receiver_ty = ResolvedTy::new(Ty::obj_name(semantic_classifier.classifier))
            .map_err(|error| self.failure(None, BodyCheckFailureKind::UnpublishableType(error)))?;
        self.body
            .add_implicit_receiver_capture(FirImplicitReceiverCapture {
                origin,
                enclosing_depth: source.enclosing_depth,
                current: source.current,
                depth: source.depth,
                path: Box::new([]),
                ty: receiver_ty,
            });
        let receiver = self.body.add_expr(FirExpr {
            origin,
            ty: receiver_ty,
            kind: FirExprKind::CapturedImplicitReceiver {
                enclosing_depth: source.enclosing_depth,
                current: source.current,
                depth: source.depth,
                path: Box::new([]),
            },
        });
        let mut classifier = current;
        let mut path = Vec::new();
        while classifier != binding.owner {
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
        Ok(Some((receiver, path.into_boxed_slice())))
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

    /// Publish a resolver-selected direct storage read with its stable owner and, inside a local
    /// callable, the exact lifted dispatch receiver that supplies that storage.
    pub(super) fn direct_class_storage_read_kind(
        &mut self,
        field: u32,
        ty: ResolvedTy,
        origin: OriginId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let owner = self
            .current_storage_owner()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingStableCallTarget))?;
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
        self.class_storage_read_kind(
            ClassCaptureBinding {
                owner,
                field,
                ty,
                shared_cell: false,
                enclosing_depth: 0,
                semantic_receiver_depth: None,
                receiver_source,
                capture_identity: None,
            },
            origin,
        )
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
    ) -> Result<Option<FirLocalClassCaptureSource>, BodyCheckFailure> {
        if self.body.local_callable().is_none() || receiver_depth < self.owned_receiver_count {
            return Ok(None);
        }
        let (enclosing_depth, depth, path) = self
            .enclosing_receiver_capture(receiver_depth as usize)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        let current = depth == 0;
        self.body
            .add_implicit_receiver_capture(FirImplicitReceiverCapture {
                origin,
                enclosing_depth,
                current,
                depth,
                path: path.clone(),
                ty,
            });
        Ok(Some(FirLocalClassCaptureSource::CapturedImplicitReceiver {
            enclosing_depth,
            current,
            depth,
            path,
        }))
    }

    pub(super) fn class_storage_read_kind(
        &mut self,
        binding: ClassCaptureBinding,
        origin: OriginId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        if self.constructor_prefix_capture_access
            && binding.enclosing_depth == 0
            && self
                .index
                .declaration_anchor(DeclarationId::from_raw(self.body.owner().raw()))
                .filter(|anchor| anchor.kind == DeclarationKind::Constructor)
                .and_then(|anchor| anchor.owner)
                == Some(binding.owner)
        {
            return Ok(FirExprKind::ConstructorCaptureRead {
                owner: binding.owner,
                field: binding.field,
                shared_cell: binding.shared_cell,
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
        if self.constructor_prefix_capture_access
            && binding.enclosing_depth == 0
            && self
                .index
                .declaration_anchor(DeclarationId::from_raw(self.body.owner().raw()))
                .filter(|anchor| anchor.kind == DeclarationKind::Constructor)
                .and_then(|anchor| anchor.owner)
                == Some(binding.owner)
        {
            return Ok(FirExprKind::ConstructorCaptureSharedWrite {
                owner: binding.owner,
                field: binding.field,
                value,
                conversion,
            });
        }
        if let Some((receiver, path)) = self.captured_class_storage_receiver(binding, origin)? {
            return Ok(FirExprKind::CapturedClassStorageSharedWrite {
                owner: binding.owner,
                receiver,
                path,
                field: binding.field,
                value,
                conversion,
            });
        }
        Ok(FirExprKind::ClassStorageSharedWrite {
            owner: binding.owner,
            enclosing_depth: binding.enclosing_depth,
            field: binding.field,
            value,
            conversion,
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
            let capture_identity = capture.capture_dependency.or(Some(ClassCaptureIdentity {
                owner: capture_owner,
                field,
            }));
            let mut ty = self.resolved_type(span, capture.ty)?;
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
                        let source = if depth == u32::MAX {
                            FirLocalClassCaptureSource::Value(storage.value)
                        } else {
                            self.body.add_capture(FirCapture {
                                origin,
                                enclosing_depth: depth,
                                source: storage.value,
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
                            capture_identity,
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
                                capture_identity,
                            },
                        );
                        if let Some(enclosing_depth) = enclosing_depth {
                            self.body.add_capture(FirCapture {
                                origin,
                                enclosing_depth,
                                source: binding.value,
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
                    } else if let Some(binding) = capture
                        .capture_dependency
                        .and_then(|identity| {
                            self.class_capture_values
                                .iter()
                                .map(|(_, binding)| *binding)
                                .find(|binding| binding.capture_identity == Some(identity))
                        })
                        .or_else(|| self.class_values.get(&capture.name).copied())
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
                                capture_identity,
                            },
                        );
                        if let Some((receiver, path)) =
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
                AnonymousObjectCaptureSource::ClassStorage {
                    field: source_field,
                } => {
                    let source_binding = self.class_values.get(&capture.name).copied();
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
                            capture_identity,
                        },
                    );
                    if let Some(binding) = source_binding {
                        if let Some((receiver, path)) =
                            self.captured_class_storage_receiver(binding, origin)?
                        {
                            FirLocalClassCaptureSource::CapturedClassStorage {
                                owner: binding.owner,
                                receiver,
                                path,
                                field: source_field,
                            }
                        } else {
                            FirLocalClassCaptureSource::ClassStorage {
                                owner: binding.owner,
                                enclosing_depth: 0,
                                field: binding.field,
                            }
                        }
                    } else {
                        let owner = self.current_storage_owner().ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        })?;
                        FirLocalClassCaptureSource::ClassStorage {
                            owner,
                            enclosing_depth: 0,
                            field: source_field,
                        }
                    }
                }
                AnonymousObjectCaptureSource::EnclosingInstance { current, depth } => {
                    let semantic_receiver_depth = depth.checked_add(1).ok_or_else(|| {
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
                        capture_identity: None,
                    });
                    if let Some(binding) = self
                        .class_receiver_binding_at(depth)
                        .filter(|binding| binding.ty == ty)
                    {
                        if let Some((receiver, path)) =
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
                    } else if let Some(source) =
                        self.captured_callable_receiver_source(origin, depth, ty)?
                    {
                        source
                    } else if current && depth == 0 {
                        FirLocalClassCaptureSource::DispatchReceiver
                    } else if let Some(path) =
                        self.enclosing_receiver_path(&crate::resolve::ImplicitReceiverSelection {
                            ty: capture.ty,
                            current,
                            receiver_depth: depth as usize,
                            classifier: None,
                            context_binding: None,
                            singleton: None,
                        })
                    {
                        FirLocalClassCaptureSource::EnclosingReceiver { path }
                    } else {
                        FirLocalClassCaptureSource::ImplicitReceiver { current, depth }
                    }
                }
                AnonymousObjectCaptureSource::ImplicitReceiver { current, depth } => {
                    let semantic_receiver_depth = depth.checked_add(1).ok_or_else(|| {
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
                        capture_identity: None,
                    });
                    if let Some(binding) = self
                        .class_receiver_binding_at(depth)
                        .filter(|binding| binding.ty == ty)
                    {
                        if let Some((receiver, path)) =
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
                    } else if let Some(source) =
                        self.captured_callable_receiver_source(origin, depth, ty)?
                    {
                        source
                    } else {
                        FirLocalClassCaptureSource::ImplicitReceiver { current, depth }
                    }
                }
            };
            checked.push(FirLocalClassCapture {
                origin,
                name: capture.name.clone().into_boxed_str(),
                ty,
                shared_cell: capture.shared_cell,
                source,
            });
        }
        if !context.values.is_empty()
            || !context.delegates.is_empty()
            || !context.callables.is_empty()
            || !context.receivers.is_empty()
            || context.enclosing_property.is_some()
        {
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
        }
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
        let captures = self.checked_class_captures(declaration, span, origin, &captures)?;
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
        Ok(FirExprKind::AnonymousObject(FirAnonymousObject {
            declaration,
            captures,
            delegate_arguments: delegate_arguments.into_boxed_slice(),
        }))
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
        if let Some(binding) =
            stable_classifier_binding.or_else(|| self.class_receiver_binding_at(selected_depth))
        {
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
        let Some(captures) = transient
            .and_then(|declaration| self.info.local_class_captures_by_class.get(&declaration))
        else {
            return Ok(None);
        };
        let Some(source_depth) = selected_depth.checked_sub(self.owned_receiver_count) else {
            return Ok(None);
        };
        let Some((field, capture)) = captures
            .iter()
            .enumerate()
            .find(|(_, capture)| match capture.source {
                AnonymousObjectCaptureSource::ImplicitReceiver { depth, .. } => {
                    depth == source_depth
                }
                AnonymousObjectCaptureSource::EnclosingInstance { depth, .. } => {
                    depth == source_depth
                }
                AnonymousObjectCaptureSource::LexicalValue
                | AnonymousObjectCaptureSource::ClassStorage { .. } => false,
            })
        else {
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
            capture_identity: None,
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
        let captures = self.checked_class_captures(declaration, class.span, origin, &captures)?;
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::LocalDeclaration {
                declaration,
                captures,
            },
        }))
    }
}
