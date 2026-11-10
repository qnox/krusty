//! Values a class body binds ahead of its dispatch rung: the primary-constructor parameters its
//! initializers see and the storage of values a local class captures.

use super::{
    AnonymousObjectCapture, AnonymousObjectCaptureSource, Checker, CheckerModuleSymbols,
    CheckerScope, ClassSig, ClassStorageSource, DispatchPropertyBinding, EnumEntryPropertyBinding,
    ErrorProvenance, ReceiverFnValueOrigin, ScopedProperty, SourceFragmentMode,
    ValueBindingDeclaration,
};
use crate::ast::{Decl, DeclId, PropParam};
use crate::types::Ty;

impl Checker<'_> {
    /// Captures selected by the current check, or by retained declaration-owned discovery.
    /// Signature defaults export the latter into their own syntax-bound result before FIR uses it.
    pub(super) fn body_class_storage_captures(
        &mut self,
        declaration: DeclId,
    ) -> Vec<AnonymousObjectCapture> {
        if let Some(captures) = self
            .discovered_local_class_captures
            .get(&declaration)
            .or_else(|| self.discovered_anonymous_captures.get(&declaration))
        {
            return captures.clone();
        }
        if self.fragment == SourceFragmentMode::SignatureDefaults {
            let Some(captures) = self.completed_retained_class_captures(declaration) else {
                return Vec::new();
            };
            if self
                .file
                .anonymous_object_classes
                .values()
                .any(|&owner| owner == declaration)
            {
                self.discovered_anonymous_captures
                    .insert(declaration, captures.clone());
                self.finalized_anonymous_captures.insert(declaration);
            } else if self.file.is_local_declaration(declaration) {
                self.discovered_local_class_capture_bindings.insert(
                    declaration,
                    Self::local_capture_binding_identities(&captures),
                );
                self.discovered_local_class_captures
                    .insert(declaration, captures.clone());
                self.finalized_local_class_captures.insert(declaration);
            }
            return captures;
        }
        self.retained_class_storage_captures(declaration)
    }

    /// The complete Pass-1 producer owns both the selected vector and its constructor carriers.
    /// Follow the active stable declaration across syntax compaction; absence proves nothing.
    fn completed_retained_class_captures(
        &self,
        declaration: DeclId,
    ) -> Option<Vec<AnonymousObjectCapture>> {
        let CheckerModuleSymbols::Legacy(source) = &self.module else {
            return None;
        };
        let symbols = source.pass_one_symbols();
        let (original, class) = self.retained_class_capture_owner(declaration)?;
        let key = (self.file_index, original);
        if !symbols.anonymous_object_capture_discovered.contains(&key) {
            return None;
        }
        let captures = symbols.anonymous_object_captures.get(&key)?;
        let first = class.ctor_params.len().checked_sub(captures.len())?;
        if class.ctor_param_shapes.len() != class.ctor_params.len()
            || !class.ctor_params[first..]
                .iter()
                .zip(&class.ctor_param_shapes[first..])
                .zip(captures)
                .all(|((ty, shape), capture)| {
                    *ty == capture.stored_ty() && *shape == (capture.stored_ty(), false)
                })
        {
            return None;
        }
        Some(captures.clone())
    }

    fn retained_class_capture_owner(&self, declaration: DeclId) -> Option<(DeclId, &ClassSig)> {
        let CheckerModuleSymbols::Legacy(source) = &self.module else {
            return None;
        };
        let stable = self
            .active_declarations?
            .classifier_declaration(declaration)?;
        let class = source
            .pass_one_symbols()
            .class_by_stable_declaration(stable)?;
        (class.source_file == self.file_index).then_some((class.source_decl?, class))
    }

    fn retained_class_storage_captures(&self, declaration: DeclId) -> Vec<AnonymousObjectCapture> {
        let CheckerModuleSymbols::Legacy(source) = &self.module else {
            return Vec::new();
        };
        source
            .pass_one_symbols()
            .anonymous_object_captures
            .get(&(self.file_index, declaration))
            .or_else(|| {
                // Retained inline syntax is compacted before its checked FIR is detached, so its
                // parser-local declaration numbers may differ from the capture-discovery arena.
                // Both arenas are already bound to the same stable classifier declaration. Follow
                // that identity back to the still-live Pass-1 capture record; no source range or
                // spelling is used, and this path disappears with the Pass-1 AST.
                let (original, _) = self.retained_class_capture_owner(declaration)?;
                source
                    .pass_one_symbols()
                    .anonymous_object_captures
                    .get(&(self.file_index, original))
            })
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn declare_scoped_property(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &ScopedProperty,
        owner_storage_visible: bool,
    ) {
        self.declare_scoped_property_with_mutability(
            scope,
            property,
            owner_storage_visible,
            property.is_var,
        );
    }

    pub(super) fn declare_scoped_property_with_mutability(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &ScopedProperty,
        owner_storage_visible: bool,
        is_var: bool,
    ) {
        if let Some((selected, span)) = property.enum_entry_property {
            if property
                .class_storage
                .is_some_and(|field| field != selected.sibling)
            {
                self.diags.error(
                    span,
                    "declared property storage has no enum-entry declaration role",
                );
                return;
            }
            if self
                .declare_enum_entry_dispatch_property(
                    scope,
                    &property.name,
                    property.ty,
                    is_var,
                    EnumEntryPropertyBinding {
                        owner: property.owner,
                        property: selected,
                        receiver: property.dispatch_receiver_identity,
                    },
                )
                .is_err()
            {
                self.diags.error(
                    span,
                    "declared enum-entry property has no selected receiver",
                );
            }
            return;
        }
        if property.class_storage.is_some() {
            self.diags.error(
                crate::diag::Span::new(0, 0),
                "declared property storage has no enum-entry declaration role",
            );
            return;
        }
        let error_provenance = if property.ty == Ty::Error
            && self
                .source_member_inference_failed(property.source_member, property.stable_declaration)
        {
            ErrorProvenance::Diagnosed
        } else {
            ErrorProvenance::None
        };
        let storage = self.visible_owner_storage_type(scope, property, owner_storage_visible);
        self.declare_dispatch_property_with_provenance(
            scope,
            &property.name,
            DispatchPropertyBinding {
                read_ty: storage.unwrap_or(property.ty),
                declared_ty: property.ty,
                error_provenance,
                owner_storage: storage.is_some(),
            },
            is_var,
            property.owner,
            None,
            property.dispatch_receiver_identity,
        );
    }

    pub(super) fn declare_class_storage(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
        storage: ClassStorageSource,
        shared_cell: bool,
    ) {
        self.declare_with_origin(
            scope,
            name,
            ty,
            is_var,
            ValueBindingDeclaration::ordinary(
                ReceiverFnValueOrigin::ClassStorage(storage),
                ErrorProvenance::None,
            )
            .in_shared_cell(shared_cell),
        );
    }

    pub(super) fn body_class_storage_owner(
        &self,
        declaration: DeclId,
    ) -> Option<crate::types::TypeName> {
        let Decl::Class(class) = self.file.decl(declaration) else {
            return None;
        };
        self.active_classifier_internal(declaration, class)
    }

    pub(super) fn body_class_declaration(
        &self,
        source: Option<&crate::libraries::SourceMember>,
    ) -> Option<DeclId> {
        match source {
            Some(crate::libraries::SourceMember::Class { file, owner, .. })
                if *file == self.file_index =>
            {
                Some(DeclId(*owner))
            }
            _ => None,
        }
    }

    /// Declare the primary-constructor parameters visible in a property initializer or an `init`
    /// block. Kotlin 2 resolves a `val`/`var` parameter named there to the property it declares, so
    /// only plain parameters become values of this rung; supertype arguments, delegation
    /// expressions and parameter defaults declare every parameter instead. A script's parameters
    /// are not in its statements' scope at all.
    pub(super) fn declare_initializer_parameters(
        &mut self,
        scope: &CheckerScope<'_>,
        parameters: &[PropParam],
        types: impl IntoIterator<Item = Ty>,
    ) {
        for (parameter, ty) in parameters.iter().zip(types) {
            if !parameter.is_property && !parameter.is_script_parameter {
                self.declare(scope, &parameter.name, ty, parameter.is_var);
                self.record_lexical_declaration(
                    scope,
                    &parameter.name,
                    super::lexical_bindings::LexicalValueRole::Parameter(
                        parameter.declaration_span,
                    ),
                );
            }
        }
    }

    /// Bind the lexical capture selected for one property's own initializer. That initializer is
    /// the sole class-body region where an enclosing value with the same spelling outranks the
    /// property being initialized; accessors and sibling member bodies retain the property rung.
    pub(super) fn declare_property_initializer_class_storage_capture(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        name: &str,
    ) {
        let Some(capture) = self
            .body_class_storage_captures(declaration)
            .into_iter()
            .find(|capture| {
                capture.name == name
                    && capture.enum_entry_storage.is_none()
                    && matches!(
                        capture.source,
                        AnonymousObjectCaptureSource::LexicalValue
                            | AnonymousObjectCaptureSource::ClassStorage(_)
                            | AnonymousObjectCaptureSource::EnumEntryPropertyStorage { .. }
                    )
                    && capture.storage_ty.is_none()
            })
        else {
            return;
        };
        let Some(owner) = self.body_class_storage_owner(declaration) else {
            if let Decl::Class(class) = self.file.decl(declaration) {
                self.diags
                    .error(class.span, "class storage has no checked classifier owner");
            }
            return;
        };
        let Some(identity) = capture.logical_identity(owner) else {
            self.diags.error(
                crate::diag::Span::new(0, 0),
                "class storage has no selected logical capture identity",
            );
            return;
        };
        self.declare_class_storage(
            scope,
            &capture.name,
            capture.ty,
            capture.shared_cell,
            super::ClassStorageSource { owner, identity },
            capture.shared_cell,
        );
        self.retain_capture_declaration(scope, &capture);
    }

    /// Shadow synthetic immutable capture properties with their semantic source bindings. A shared
    /// cell remains writable inside methods and accessors even though the field holding the cell is
    /// itself final. Kotlin's scope tower also keeps a function-local value ahead of a same-named
    /// member of a local class, so a colliding capture remains visible in member bodies.
    pub(super) fn declare_body_class_storage_captures(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
    ) {
        for capture in self.body_class_storage_captures(declaration) {
            if capture.enum_entry_storage.is_some() {
                if !self.declare_captured_enum_property(scope, &capture) {
                    if let Decl::Class(class) = self.file.decl(declaration) {
                        self.diags.error(
                            class.span,
                            "selected enum-entry property has no declaring receiver",
                        );
                    }
                }
                continue;
            }
            if matches!(
                capture.source,
                AnonymousObjectCaptureSource::LexicalValue
                    | AnonymousObjectCaptureSource::ClassStorage(_)
                    | AnonymousObjectCaptureSource::EnumEntryPropertyStorage { .. }
            ) && capture.storage_ty.is_none()
            {
                let Some(owner) = self.body_class_storage_owner(declaration) else {
                    if let Decl::Class(class) = self.file.decl(declaration) {
                        self.diags
                            .error(class.span, "class storage has no checked classifier owner");
                    }
                    return;
                };
                let Some(identity) = capture.logical_identity(owner) else {
                    self.diags.error(
                        crate::diag::Span::new(0, 0),
                        "class storage has no selected logical capture identity",
                    );
                    return;
                };
                self.declare_class_storage(
                    scope,
                    &capture.name,
                    capture.ty,
                    capture.shared_cell,
                    super::ClassStorageSource { owner, identity },
                    capture.shared_cell,
                );
                self.retain_capture_declaration(scope, &capture);
            }
        }
    }
}
