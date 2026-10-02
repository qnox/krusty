//! Values a class body binds ahead of its dispatch rung: the primary-constructor parameters its
//! initializers see and the storage of values a local class captures.

use super::{AnonymousObjectCaptureSource, Checker, CheckerScope};
use crate::ast::{Decl, DeclId, PropParam};
use crate::types::Ty;

impl Checker<'_> {
    /// Declare the primary-constructor parameters visible in a property initializer or an `init`
    /// block. Kotlin 2 resolves a `val`/`var` parameter named there to the property it declares, so
    /// only plain parameters become values of this rung; supertype arguments, delegation
    /// expressions and parameter defaults declare every parameter instead.
    pub(super) fn declare_initializer_parameters(
        &mut self,
        scope: &CheckerScope<'_>,
        parameters: &[PropParam],
        types: impl IntoIterator<Item = Ty>,
    ) {
        for (parameter, ty) in parameters.iter().zip(types) {
            if !parameter.is_property {
                self.declare(scope, &parameter.name, ty, parameter.is_var);
            }
        }
    }

    fn body_class_declares_property_named(&self, declaration: DeclId, name: &str) -> bool {
        matches!(self.file.decl(declaration), Decl::Class(class) if class
            .props
            .iter()
            .map(|property| property.name.as_str())
            .chain(class.body_props.iter().map(|property| property.name.as_str()))
            .any(|property| property == name))
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
    /// itself final. A source property keeps its dispatch identity; only its own initializer may
    /// explicitly select the same-named enclosing capture above that rung.
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
                && !self.body_class_declares_property_named(declaration, &capture.name)
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
