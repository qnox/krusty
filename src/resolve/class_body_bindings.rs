//! Values a class body binds ahead of its dispatch rung: the primary-constructor parameters its
//! initializers see and the storage of values a local class captures.

use super::{AnonymousObjectCaptureSource, Checker, CheckerScope};
use crate::ast::{DeclId, PropParam};
use crate::types::Ty;

impl Checker<'_> {
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
                            | AnonymousObjectCaptureSource::ClassStorage { .. }
                    )
                    && capture.storage_ty.is_none()
            })
        else {
            return;
        };
        self.declare_class_storage(
            scope,
            &capture.name,
            capture.ty,
            capture.shared_cell,
            u32::try_from(field).expect("too many local-class captures"),
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
                    | AnonymousObjectCaptureSource::ClassStorage { .. }
            ) && capture.storage_ty.is_none()
            {
                self.declare_class_storage(
                    scope,
                    &capture.name,
                    capture.ty,
                    capture.shared_cell,
                    field as u32,
                    capture.shared_cell,
                );
            }
        }
    }
}
