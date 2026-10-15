//! Selecting the field-visible type and storage operation for explicit backing-field reads.
//!
//! The property keeps its public type outside its declaring class. Inside that class, an exact
//! owner instance exposes the narrower field type; nested classes and inherited properties still
//! use the getter. These decisions consume stable receiver and owner identities from the scope
//! tower rather than recovering them from source spelling.

use super::*;

impl Checker<'_> {
    /// Check explicit backing-field initializer bodies and publish the storage type visible to
    /// subsequent member bodies. A non-local property's storage type is a finalized Pass-1 fact;
    /// this pass uses its stable declaration identity and never derives that fact from body syntax.
    /// Body-local classes are the exception because their signatures deliberately remain on the
    /// lexical body rung until this bounded class check.
    pub(super) fn check_explicit_backing_field_initializers(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        declaration: DeclId,
        current_owner: Option<TypeName>,
        body_local_class: bool,
        properties: &mut [ScopedProperty],
    ) {
        let field_scope = scope.child(ScopeKind::Block);
        let field_scope = &field_scope;
        for property in properties.iter() {
            self.declare_scoped_property(field_scope, property, false);
        }
        let primary_parameter_types = current_owner
            .and_then(|owner| self.fed_source().classifier(owner))
            .and_then(|classifier| {
                classifier
                    .constructors
                    .first()
                    .and_then(|constructor| constructor.params.get(..class.props.len()))
                    .map(|parameters| parameters.to_vec())
            })
            .unwrap_or_else(|| {
                class
                    .props
                    .iter()
                    .map(|parameter| self.type_ref_ty(field_scope, &parameter.ty))
                    .collect()
            });
        self.declare_initializer_parameters(field_scope, &class.props, primary_parameter_types);

        for (property_index, property) in class.body_props.iter().enumerate() {
            let Some(field) = property.explicit_backing_field.as_ref() else {
                continue;
            };
            let selected = self
                .selected_body_declarations
                .as_ref()
                .is_none_or(|selected| selected.contains(&property.span));
            if !selected {
                continue;
            }
            let source_member = crate::libraries::SourceMember::ClassProperty {
                file: self.file_index,
                owner: declaration.0,
                property: (class.props.len() + property_index) as u32,
            };
            let stable_declaration = self.active_declarations.map(|active| {
                active
                    .class_body_property_declaration(
                        declaration,
                        u32::try_from(property_index).expect("too many class properties"),
                    )
                    .expect("active class body property has no stable declaration")
            });
            let scoped_index = properties
                .iter()
                .position(|candidate| match stable_declaration {
                    Some(stable) => candidate.stable_declaration == Some(stable),
                    None => candidate.source_member == Some(source_member),
                });
            let declared = scoped_index
                .map(|index| properties[index].ty)
                .or_else(|| {
                    stable_declaration
                        .zip(self.resolved_index)
                        .and_then(|(stable, index)| index.signature(stable))
                        .map(|signature| signature.result.get())
                })
                .filter(|ty| !ty.mentions_error() && !ty.mentions_pending());

            let property_suppression_depth = self.push_declaration_policies(
                field_scope,
                &property.annotations,
                &property.annotation_args,
            );
            let initializer_scope = field_scope.child(ScopeKind::Block);
            if body_local_class {
                self.declare_property_initializer_class_storage_capture(
                    &initializer_scope,
                    declaration,
                    &property.name,
                );
            }
            let checked_storage = match (&field.ty, property.init) {
                (Some(reference), _) => self.check_declaration_type(field_scope, reference),
                // kotlinc resolves the initializer against the property's public type. Pass 1
                // owns the resulting storage type; Pass 2 checks only the executable body.
                (None, Some(initializer)) => match declared {
                    Some(expected) => self.expr_expected(&initializer_scope, initializer, expected),
                    None => self.expr(&initializer_scope, initializer),
                },
                (None, None) => Ty::Error,
            };
            if let Some(initializer) = property.init {
                self.finish_executable_root(initializer);
            }
            let storage = if body_local_class || self.active_declarations.is_none() {
                checked_storage
            } else {
                self.stable_property_storage_type(stable_declaration)
                    .unwrap_or(Ty::Error)
            };
            if !storage.mentions_error() && !storage.mentions_pending() {
                self.explicit_backing_field_types
                    .insert((property.span.lo, property.span.hi), storage);
                if let Some(scoped_index) = scoped_index {
                    let scoped = &mut properties[scoped_index];
                    let declared = scoped.ty;
                    scoped.owner_storage_ty = Some(storage);
                    self.declare_scoped_property(field_scope, scoped, true);
                    if field.ty.is_none()
                        && !crate::assignable::is_subtype(
                            &crate::assignable::TyCtx::new(),
                            self,
                            storage,
                            declared,
                        )
                    {
                        self.diags.error(
                            property.span,
                            format!(
                                "backing field type of '{}' is '{}', which is not a subtype of its property type '{}'.",
                                property.name,
                                storage.source_name(),
                                declared.source_name(),
                            ),
                        );
                    }
                }
            }
            self.active_lexical_policies
                .truncate(property_suppression_depth);
        }
    }

    /// A qualified read sees an explicit backing field's type when a lexical binding of this
    /// property carries that field and the receiver's static type is exactly the declaring class.
    /// The field is loaded only when this expression is compiled into that class (`other.a` and
    /// `this.a` are `getfield`; a local function and a lambda are too). A nested or inner class
    /// keeps the field's type but calls the getter, which returns the public type and is then
    /// checked back. A subclass value and an inherited property stay on the public type and getter.
    pub(super) fn qualified_owner_storage_type(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        receiver_ty: Ty,
        name: &str,
    ) -> Option<Ty> {
        let binding = scope.ancestors().find_map(|rung| {
            let local = rung.own_binding(name, Ns::Value)?.value()?;
            match local.origin {
                ReceiverFnValueOrigin::DispatchProperty {
                    owner,
                    owner_storage: true,
                    ..
                } if receiver_ty.non_null().obj_internal() == Some(owner) => Some(local),
                _ => None,
            }
        })?;
        let ReceiverFnValueOrigin::DispatchProperty {
            declared_ty,
            owner: binding_owner,
            receiver_identity,
            ..
        } = binding.origin
        else {
            return None;
        };
        let Some(ExprLowering::MemberPropertyRead {
            owner,
            owner_storage,
            ..
        }) = self.expr_lowers.get_mut(&expression)
        else {
            return None;
        };
        if *owner != binding_owner {
            return None;
        }
        let compiled_into_owner = scope
            .innermost_class_receiver_identity()
            .is_none_or(|class_identity| class_identity == receiver_identity);
        if compiled_into_owner {
            *owner_storage = true;
        }
        (binding.ty != declared_ty).then_some(binding.ty)
    }

    /// The narrower backing-field type visible at the declaration site, if this scope belongs to
    /// the declaring class. A recursively collected inherited property never satisfies that exact
    /// owner check.
    pub(super) fn visible_owner_storage_type(
        &self,
        scope: &CheckerScope<'_>,
        property: &ScopedProperty,
        owner_storage_visible: bool,
    ) -> Option<Ty> {
        if !owner_storage_visible {
            return None;
        }
        let declaring_class = scope
            .innermost_class_receiver_identity()
            .and_then(|identity| {
                self.implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| receiver.identity == identity)
                    .and_then(|receiver| receiver.ty.non_null().obj_internal())
            });
        if declaring_class != Some(property.owner) {
            return None;
        }
        // While the field initializer is being determined, its type is the pending marker. The
        // property's already-known public type remains usable, so do not leak that marker.
        property
            .owner_storage_ty
            .filter(|storage| !storage.mentions_pending())
    }
}
