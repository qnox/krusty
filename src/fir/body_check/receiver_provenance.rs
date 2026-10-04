//! Checked receiver-frame identity carried across nested callable boundaries.

use std::collections::HashMap;

use crate::fir::CapturedCallableOwner;

use super::*;

#[derive(Clone, Debug)]
pub(super) struct ReceiverFrame {
    /// Width in resolver receiver-tower coordinates. Named context values are ordinary lexical
    /// bindings and therefore do not occupy receiver-tower or runtime receiver slots.
    pub(super) width: u32,
    /// Stable owner of this frame's dispatch receiver. Enum entries are classifier-like semantic
    /// owners even though their anonymous runtime subclass is not a source classifier header.
    pub(super) dispatch_owner: Option<DeclarationId>,
    /// Runtime receiver-slot coordinate of `dispatch_owner` inside this frame.
    pub(super) dispatch_depth: Option<u32>,
    /// Resolver coordinate to runtime receiver-slot coordinate for capturable receivers. Named
    /// context values are absent because their stable `context_binding` captures the value.
    capture_depths: HashMap<u32, u32>,
    /// Source-semantic identity of each capturable receiver at the same resolver coordinate.
    /// This is selected while the declaring body and its context rung are still available.
    capture_receivers: HashMap<u32, FirCapturedReceiver>,
    /// Receiver coordinates in this frame that are reached through checked enclosing-instance
    /// edges rather than direct callable slots.
    structural_paths: HashMap<u32, Box<[DeclarationId]>>,
}

impl BodyFirChecker<'_> {
    /// Semantic receiver frame exposed to a nested callable. Direct callable receivers are ordinary
    /// slots. A non-local member body can additionally expose outer instances through an `inner`
    /// classifier chain; publish the exact declaration path for each such coordinate so a nested
    /// capture never degrades it to type/depth lookup in lowering.
    pub(super) fn receiver_frame(&self) -> ReceiverFrame {
        let mut structural_paths = HashMap::new();
        let mut capture_depths = HashMap::new();
        let mut capture_receivers = HashMap::new();
        let extension_count = u32::from(self.body.receiver_type().is_some());
        let context_count = u32::try_from(self.body.context_receiver_types().len())
            .expect("too many checked-body context receivers");
        let mut semantic_depth = 0;
        let mut runtime_depth = 0;
        if extension_count != 0 {
            capture_depths.insert(semantic_depth, runtime_depth);
            capture_receivers.insert(semantic_depth, self.extension_receiver_capture());
            semantic_depth += 1;
            runtime_depth += 1;
        }
        for declaration_ordinal in (0..context_count).rev() {
            if !self
                .body
                .is_context_value_ordinal(declaration_ordinal as usize)
            {
                capture_depths.insert(semantic_depth, runtime_depth);
                capture_receivers.insert(
                    semantic_depth,
                    self.context_receiver_capture(declaration_ordinal as usize),
                );
                semantic_depth += 1;
                runtime_depth += 1;
            }
        }
        if self.body.local_callable().is_some() {
            return ReceiverFrame {
                width: self.owned_receiver_count,
                dispatch_owner: None,
                dispatch_depth: None,
                capture_depths,
                capture_receivers,
                structural_paths,
            };
        }
        let dispatch_owner = self.current_storage_owner();
        let dispatch_depth = dispatch_owner.map(|_| {
            capture_depths.insert(semantic_depth, runtime_depth);
            capture_receivers.insert(semantic_depth, FirCapturedReceiver::Enclosing);
            runtime_depth
        });
        let owner = DeclarationId::from_raw(self.body.owner().raw());
        let mut classifier = self
            .index
            .enclosing_classifier(owner)
            .map(|classifier| classifier.declaration);
        let mut path = Vec::new();
        while let Some(current) = classifier {
            let Some(header) = self.index.declaration_header(current) else {
                break;
            };
            if !header.flags.has(crate::fir::DeclarationFlags::INNER) {
                break;
            }
            let Some(outer) = self
                .index
                .declaration_anchor(current)
                .and_then(|anchor| anchor.owner)
                .filter(|owner| self.index.classifier_header(*owner).is_some())
            else {
                break;
            };
            path.push(current);
            let depth = self
                .owned_receiver_count
                .checked_add(
                    u32::try_from(structural_paths.len())
                        .expect("too many structural receiver paths"),
                )
                .expect("too many implicit receivers");
            structural_paths.insert(depth, path.clone().into_boxed_slice());
            capture_depths.insert(depth, depth);
            capture_receivers.insert(depth, FirCapturedReceiver::Enclosing);
            classifier = Some(outer);
        }
        ReceiverFrame {
            width: self
                .owned_receiver_count
                .checked_add(
                    u32::try_from(structural_paths.len())
                        .expect("too many structural receiver paths"),
                )
                .expect("too many implicit receivers"),
            dispatch_owner,
            dispatch_depth,
            capture_depths,
            capture_receivers,
            structural_paths,
        }
    }

    fn extension_receiver_capture(&self) -> FirCapturedReceiver {
        if let Some(lambda) = self.body.source_lambda() {
            return FirCapturedReceiver::Lambda(lambda.label().map(Box::<str>::from));
        }
        if let Some(name) = self.body.debug_name() {
            let owner = match self.body.local_callable() {
                Some(_) => CapturedCallableOwner::LocalFunction,
                None => CapturedCallableOwner::Declaration,
            };
            return FirCapturedReceiver::Callable {
                label: name.into(),
                owner,
            };
        }
        let declaration = DeclarationId::from_raw(self.body.owner().raw());
        let property = self
            .index
            .declaration_header(declaration)
            .filter(|header| header.kind == crate::fir::DeclarationKind::Accessor)
            .and_then(|header| header.owner)
            .expect("an extension body without a name is a property accessor");
        let name = self
            .index
            .declaration_name(property)
            .expect("an extension property has a source name");
        FirCapturedReceiver::Callable {
            label: name.into(),
            owner: CapturedCallableOwner::Declaration,
        }
    }

    fn context_receiver_capture(&self, ordinal: usize) -> FirCapturedReceiver {
        use crate::types::{CapturedContextKind, ContextParameterKind};

        let function_type = self.body.source_lambda().is_some();
        let source_kind = self.body.context_parameter_kinds()[ordinal];
        let kind = if function_type {
            CapturedContextKind::FunctionType
        } else {
            match source_kind {
                ContextParameterKind::Anonymous => CapturedContextKind::Anonymous,
                ContextParameterKind::LegacyReceiver => CapturedContextKind::LegacyReceiver,
                ContextParameterKind::Named | ContextParameterKind::None => {
                    unreachable!("only implicit context receivers are capturable")
                }
            }
        };
        let same_rung = self
            .body
            .context_receiver_types()
            .iter()
            .copied()
            .zip(self.body.context_parameter_kinds().iter().copied())
            .enumerate()
            .filter(|(_, (_, candidate))| {
                if function_type {
                    *candidate != ContextParameterKind::Named
                } else {
                    *candidate == source_kind
                }
            })
            .collect::<Vec<_>>();
        let index = same_rung
            .iter()
            .position(|(candidate, _)| *candidate == ordinal)
            .expect("a captured context receiver belongs to its declared rung");
        FirCapturedReceiver::Context {
            kind,
            types: same_rung
                .into_iter()
                .map(|(_, (ty, _))| ty)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            index: u32::try_from(index).expect("too many context receivers"),
        }
    }

    /// The nearest stable declaration that owns instance storage for this body. This is a checked
    /// ownership edge, not a classifier/name search: entry-body members point directly at their
    /// stable enum-entry declaration, while ordinary members point at a classifier declaration.
    pub(super) fn current_storage_owner(&self) -> Option<DeclarationId> {
        let mut declaration = DeclarationId::from_raw(self.body.owner().raw());
        loop {
            let anchor = self.index.declaration_anchor(declaration)?;
            if matches!(
                anchor.kind,
                crate::fir::DeclarationKind::Classifier | crate::fir::DeclarationKind::EnumEntry
            ) {
                return Some(declaration);
            }
            declaration = anchor.owner?;
        }
    }

    pub(super) fn current_named_context_parameter(
        &self,
        name: &str,
    ) -> Option<(DeclarationId, u32, ResolvedTy)> {
        let owner = self.current_storage_owner()?;
        let (ordinal, parameter) = self
            .index
            .classifier_header(owner)?
            .context_parameters
            .iter()
            .enumerate()
            .find(|(_, parameter)| parameter.name.as_deref() == Some(name))?;
        Some((
            owner,
            u32::try_from(ordinal).expect("too many classifier context parameters"),
            parameter.ty,
        ))
    }

    pub(super) fn enclosing_receiver_capture(
        &self,
        receiver_depth: usize,
    ) -> Option<(u32, u32, Box<[DeclarationId]>, FirCapturedReceiver)> {
        let mut depth = receiver_depth.checked_sub(self.owned_receiver_count as usize)?;
        for (enclosing_depth, frame) in self.outer_receiver_frames.iter().enumerate() {
            if depth < frame.width as usize {
                let semantic_depth = u32::try_from(depth).ok()?;
                let captured_depth = *frame.capture_depths.get(&semantic_depth)?;
                let receiver = frame.capture_receivers.get(&semantic_depth)?.clone();
                return Some((
                    u32::try_from(enclosing_depth).expect("too many nested receiver frames"),
                    captured_depth,
                    frame
                        .structural_paths
                        .get(&semantic_depth)
                        .cloned()
                        .unwrap_or_default(),
                    receiver,
                ));
            }
            depth = depth.checked_sub(frame.width as usize)?;
        }
        None
    }

    /// Exact source role of one scoped receiver-tower coordinate. This is queried only while the
    /// checked body and all enclosing receiver frames are live, then retained by consumers that
    /// need to carry the selected receiver across a lowering boundary.
    pub(super) fn receiver_capture_at_depth(
        &self,
        receiver_depth: usize,
    ) -> Option<FirCapturedReceiver> {
        if receiver_depth < self.owned_receiver_count as usize {
            let semantic_depth = u32::try_from(receiver_depth).ok()?;
            return self
                .receiver_frame()
                .capture_receivers
                .get(&semantic_depth)
                .cloned();
        }
        self.enclosing_receiver_capture(receiver_depth)
            .map(|(_, _, _, receiver)| receiver)
    }

    /// Translate a resolver receiver-tower coordinate beyond this callable's own receiver slots
    /// into the exact semantic `inner`-classifier path that supplies it at runtime. This publishes
    /// declaration identities only; how a backend stores each enclosing instance is deliberately
    /// absent from checked FIR.
    pub(super) fn enclosing_receiver_path(
        &self,
        selected: &crate::resolve::ImplicitReceiverSelection,
    ) -> Option<Box<[DeclarationId]>> {
        // An enum-entry body exposes both the anonymous entry receiver and its parent-enum view as
        // receiver-tower rungs, but they are the same runtime dispatch object. Publish that exact
        // alias as the zero-edge enclosing path; lowering then reads the current dispatch slot and
        // performs no tower interpretation of its own.
        if let Some(entry) = self.current_storage_owner().filter(|owner| {
            self.index
                .declaration_anchor(*owner)
                .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::EnumEntry)
        }) {
            let parent = self
                .index
                .declaration_anchor(entry)?
                .owner
                .and_then(|owner| self.index.classifier_header(owner))?;
            if selected.ty.non_null().kotlin_class_internal() == Some(parent.classifier) {
                return Some(Box::new([]));
            }
        }
        selected
            .receiver_depth
            .checked_sub(self.owned_receiver_count as usize)?;
        let owner = DeclarationId::from_raw(self.body.owner().raw());
        let mut classifier = self.index.enclosing_classifier(owner)?.declaration;
        let selected_classifier = selected.classifier;
        let selected_type = selected.ty.non_null().kotlin_class_internal()?;
        let matches_selected = |candidate: DeclarationId| {
            selected_classifier == Some(candidate)
                || self
                    .index
                    .classifier_header(candidate)
                    .is_some_and(|header| header.classifier == selected_type)
                || self
                    .index
                    .declaration_anchor(candidate)
                    .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::EnumEntry)
                    .and_then(|anchor| anchor.owner)
                    .is_some_and(|parent| {
                        selected_classifier == Some(parent)
                            || self
                                .index
                                .classifier_header(parent)
                                .is_some_and(|header| header.classifier == selected_type)
                    })
        };
        let mut path = Vec::new();
        loop {
            if matches_selected(classifier) {
                return Some(path.into_boxed_slice());
            }
            let header = self.index.declaration_header(classifier)?;
            if !header.flags.has(crate::fir::DeclarationFlags::INNER) {
                return None;
            }
            let outer = self
                .index
                .declaration_anchor(classifier)?
                .owner
                .filter(|owner| {
                    self.index.classifier_header(*owner).is_some()
                        || self.index.declaration_anchor(*owner).is_some_and(|anchor| {
                            anchor.kind == crate::fir::DeclarationKind::EnumEntry
                        })
                })?;
            path.push(classifier);
            classifier = outer;
        }
    }
}
