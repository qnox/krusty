//! Stability and declared-type reads for smart-cast access paths.

use super::{
    Checker, CheckerScope, NarrowPath, ReceiverFnValueOrigin, Span, TopLevelPropertySelection, Ty,
};

/// Bounded view of the checker facts needed to decide whether an access path can be read twice.
pub(super) struct StablePathRead<'checker, 'source> {
    checker: &'checker Checker<'source>,
}

impl<'checker, 'source> StablePathRead<'checker, 'source> {
    pub(super) fn new(checker: &'checker Checker<'source>) -> Self {
        Self { checker }
    }

    /// The declared type of a stable access path. `None` means some read can change between the
    /// proof and its later use, so retaining a smart cast would be unsound.
    pub(super) fn ty(&self, scope: &CheckerScope<'_>, path: &NarrowPath, site: Span) -> Option<Ty> {
        let mut ty = if path.root == "this" {
            scope.this_ty()?
        } else {
            let Some(local) = self.checker.lookup(scope, &path.root) else {
                return self.top_level_ty(scope, path);
            };
            if local.is_var
                && (!path.segments.is_empty()
                    || !matches!(local.origin, ReceiverFnValueOrigin::Local)
                    || self.checker.closure_reassigned_before(&path.root, site))
            {
                return None;
            }
            if local.has_unstable_delegated_read() {
                return None;
            }
            // A bare own-member read is a dispatch-property read. Its binding already identifies
            // the receiver rung that owns it; a foreign receiver lambda may make another rung the
            // innermost `this`, but must not change this selected identity.
            if path.segments.is_empty() {
                if let ReceiverFnValueOrigin::DispatchProperty {
                    receiver_identity, ..
                } = local.origin
                {
                    let receiver = self
                        .checker
                        .implicit_receivers(scope)
                        .into_iter()
                        .find(|candidate| candidate.identity == receiver_identity)?
                        .ty;
                    return self.member_ty(receiver, &path.root);
                }
            }
            // A member/top-level property at the root of a longer path re-enters its accessor.
            if !path.segments.is_empty() && !matches!(local.origin, ReceiverFnValueOrigin::Local) {
                return None;
            }
            local.ty
        };
        for segment in &path.segments {
            ty = self.member_ty(ty, segment)?;
        }
        Some(ty)
    }

    /// A same-file top-level `val` with its compiler-default backing-field getter is stable like a
    /// local `val`. Cross-file and computed/delegated properties remain accessor reads.
    fn top_level_ty(&self, scope: &CheckerScope<'_>, path: &NarrowPath) -> Option<Ty> {
        if !path.segments.is_empty() {
            return None;
        }
        let TopLevelPropertySelection::Selected(access) =
            self.checker.select_top_level_property(scope, &path.root)
        else {
            return None;
        };
        if access.property.context_count != 0 {
            return None;
        }
        let Some(index) = self.checker.resolved_index else {
            return super::stable_path_legacy_bridge::top_level_ty(
                self.checker.file,
                self.checker.file_index,
                &access.property,
            );
        };
        let declaration = access.property.stable_declaration?;
        let anchor = index.declaration_anchor(declaration)?;
        if anchor.source.raw() != self.checker.file_index {
            return None;
        }
        let flags = index.declaration_header(declaration)?.flags;
        if flags.has(crate::fir::DeclarationFlags::MUTABLE)
            || flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER)
            || flags.has(crate::fir::DeclarationFlags::DELEGATED)
            || flags.has(crate::fir::DeclarationFlags::EXTERNAL)
            || flags.has(crate::fir::DeclarationFlags::EXPECT)
        {
            return None;
        }
        Some(access.property.ty)
    }

    /// Read one member property through `receiver`, declining every shape whose accessor can
    /// produce a different value on the second read.
    fn member_ty(&self, receiver: Ty, name: &str) -> Option<Ty> {
        let receiver = receiver.non_null();
        let owner = receiver.obj_internal()?;
        let class = self.checker.resolver().classifier(owner)?;
        let Some(index) = self.checker.resolved_index else {
            return super::stable_path_legacy_bridge::member_ty(
                &self.checker.module,
                receiver,
                owner,
                name,
                class.is_final(),
            );
        };
        let source = self.checker.fed_source();
        let callables = crate::symbol_resolver::members_in_hierarchy(&source, receiver, name);
        let property = callables
            .properties()
            .iter()
            .find(|property| property.kind == crate::libraries::PropKind::Member)?;
        let flags = index
            .declaration_header(property.stable_declaration?)?
            .flags;
        if flags.has(crate::fir::DeclarationFlags::MUTABLE)
            || flags.has(crate::fir::DeclarationFlags::CUSTOM_GETTER)
            || flags.has(crate::fir::DeclarationFlags::DELEGATED)
            || (flags.has(crate::fir::DeclarationFlags::OPEN) && !class.is_final())
            || property.context_count != 0
        {
            return None;
        }
        Some(property.ty.projection_read_ty())
    }
}
