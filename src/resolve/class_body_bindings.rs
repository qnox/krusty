//! Values a class body binds ahead of its dispatch rung: the primary-constructor parameters its
//! initializers see and the storage of values a local class captures.

use super::{
    AnonymousObjectCaptureSource, Checker, CheckerScope, ClassStorageSource,
    DispatchPropertyBinding, ErrorProvenance, ReceiverFnValueOrigin, ScopedProperty,
    ValueBindingDeclaration,
};
use crate::ast::{Decl, DeclId, PropParam};
use crate::types::Ty;

impl Checker<'_> {
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
        if let Some(field) = property.class_storage {
            if let Some(sibling) = property.enum_entry_property {
                self.declare_enum_entry_property_storage(
                    scope,
                    &property.name,
                    property.ty,
                    is_var,
                    property.owner,
                    sibling,
                );
                return;
            }
            self.declare_class_storage(
                scope,
                &property.name,
                property.ty,
                is_var,
                ClassStorageSource {
                    owner: property.owner,
                    field,
                },
                false,
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
            property.enum_entry_property,
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
        let Some((field, capture)) = self
            .body_class_storage_captures(declaration)
            .into_iter()
            .enumerate()
            .find(|(_, capture)| {
                capture.name == name
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
        self.declare_class_storage(
            scope,
            &capture.name,
            capture.ty,
            capture.shared_cell,
            super::ClassStorageSource {
                owner,
                field: u32::try_from(field).expect("too many local-class captures"),
            },
            capture.shared_cell,
        );
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
        for (field, capture) in self
            .body_class_storage_captures(declaration)
            .into_iter()
            .enumerate()
        {
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
                self.declare_class_storage(
                    scope,
                    &capture.name,
                    capture.ty,
                    capture.shared_cell,
                    super::ClassStorageSource {
                        owner,
                        field: field as u32,
                    },
                    capture.shared_cell,
                );
            }
        }
    }
}
