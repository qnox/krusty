//! Declared enum-entry properties keep their selected instance across capture boundaries.

use super::*;

/// A source-selected property coordinate, bound to its declaration owner before checked FIR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumEntryProperty {
    pub(crate) owner: Option<crate::fir::DeclarationId>,
    pub(crate) sibling: u32,
}

/// Exact entry-owned source storage selected before crossing a local-class capture boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EnumEntryStorage {
    pub(super) owner: TypeName,
    pub(super) property: EnumEntryProperty,
}

impl Local {
    pub(super) fn enum_entry_storage(self) -> Option<EnumEntryStorage> {
        match self.origin {
            ReceiverFnValueOrigin::EnumEntryPropertyStorage {
                owner,
                field,
                declaration,
            } => Some(EnumEntryStorage {
                owner,
                property: EnumEntryProperty {
                    owner: declaration,
                    sibling: field,
                },
            }),
            ReceiverFnValueOrigin::DispatchProperty {
                owner,
                enum_entry_property: Some(property),
                ..
            } => Some(EnumEntryStorage { owner, property }),
            _ => None,
        }
    }
}

pub(super) struct EnumEntryPropertyBinding {
    pub(super) owner: TypeName,
    pub(super) property: EnumEntryProperty,
    pub(super) receiver: Option<(usize, usize)>,
}

/// No lexical binding may be published without its selected receiver.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct MissingEnumEntryReceiver;

#[derive(Clone, Copy)]
struct DeclaredPropertyInput {
    property: EnumEntryProperty,
    receiver: (usize, usize),
    is_var: bool,
}

impl Checker<'_> {
    /// Entry-body properties captured by a nested `inner` classifier. Enum entries have no AST
    /// declaration of their own, so the nested class records the enum as `inner_of`; recover the
    /// intervening entry scope once and publish the same stable sibling identities used while the
    /// entry body itself is checked.
    pub(super) fn enclosing_enum_entry_properties(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        visible_properties: &[ScopedProperty],
    ) -> Vec<ScopedProperty> {
        let Some((outer_declaration, outer, entry_index, entry)) =
            enclosing_enum_entry_context(self.file, class)
        else {
            return Vec::new();
        };
        let property_scope = scope.child(ScopeKind::Block);
        let property_scope = &property_scope;
        for property in visible_properties {
            self.declare_scoped_property(property_scope, property, false);
        }
        let outer_internal = type_name(&class_internal(self.file, &outer.name));
        let entry_owner = type_name_nested_child(outer_internal, &entry.name);
        let mut properties = Vec::new();
        for (field, property) in entry
            .props
            .iter()
            .enumerate()
            .filter(|(_, property)| property.span.lo < class.span.lo)
        {
            let ty = property
                .ty
                .as_ref()
                .map(|reference| self.type_ref_ty(property_scope, reference))
                .or_else(|| {
                    property
                        .init
                        .map(|initializer| self.expr(property_scope, initializer))
                })
                .unwrap_or(Ty::Error);
            let Some(selected) =
                self.enum_entry_property_for(outer_declaration, entry_index as u32, field as u32)
            else {
                self.diags.error(
                    property.span,
                    "enum-entry property has no active declaration",
                );
                continue;
            };
            let receiver = self.enum_entry_receiver_for_declaration(scope, outer_internal);
            if self
                .declare_enum_entry_dispatch_property(
                    property_scope,
                    &property.name,
                    ty,
                    property.is_var,
                    EnumEntryPropertyBinding {
                        owner: entry_owner,
                        property: selected,
                        receiver,
                    },
                )
                .is_err()
            {
                self.diags.error(
                    property.span,
                    "selected enum-entry property has no declaring receiver",
                );
                continue;
            }
            properties.push(ScopedProperty {
                name: property.name.clone(),
                ty,
                owner_storage_ty: None,
                is_var: property.is_var,
                owner: entry_owner,
                dispatch_receiver_identity: receiver,
                class_storage: None,
                enum_entry_property: Some((selected, property.span)),
                source_visible: true,
                source_member: None,
                stable_declaration: None,
            });
        }
        properties
    }

    /// Bind the structurally selected entry and property before a checked body is published.
    pub(super) fn enum_entry_property_for(
        &self,
        class: DeclId,
        entry: u32,
        sibling: u32,
    ) -> Option<EnumEntryProperty> {
        let owner = match self.active_declarations {
            None => None,
            Some(active) => {
                active.enum_entry_property_declaration(class, entry, sibling)?;
                Some(active.enum_entry_declaration(class, entry)?)
            }
        };
        Some(EnumEntryProperty { owner, sibling })
    }

    /// Select a declaration role while publishing its binding, never from a member's spelling or
    /// from the semantic type of a later expression. Reconstructed class rungs use the same role.
    fn enum_entry_receiver_for_declaration(
        &self,
        scope: &CheckerScope<'_>,
        owner: TypeName,
    ) -> Option<(usize, usize)> {
        let role = ReceiverDeclarationRole::EnclosingClass(owner);
        let mut matches = self
            .implicit_receivers(scope)
            .into_iter()
            .filter(|receiver| receiver.class_receiver && receiver.receiver_role == Some(role));
        let first = matches.next()?;
        matches.next().is_none().then_some(first.identity)
    }

    pub(super) fn declare_enum_entry_property_storage(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
        storage: EnumEntryStorage,
    ) {
        self.declare_with_origin(
            scope,
            name,
            ty,
            is_var,
            ValueBindingDeclaration::ordinary(
                ReceiverFnValueOrigin::EnumEntryPropertyStorage {
                    owner: storage.owner,
                    field: storage.property.sibling,
                    declaration: storage.property.owner,
                },
                ErrorProvenance::None,
            ),
        );
    }

    pub(super) fn declare_enum_entry_dispatch_property(
        &mut self,
        scope: &CheckerScope<'_>,
        name: &str,
        ty: Ty,
        is_var: bool,
        binding: EnumEntryPropertyBinding,
    ) -> Result<(), MissingEnumEntryReceiver> {
        let EnumEntryPropertyBinding {
            owner,
            property,
            receiver,
        } = binding;
        let Some(receiver_identity) = receiver else {
            return Err(MissingEnumEntryReceiver);
        };
        self.declare_with_origin(
            scope,
            name,
            ty,
            is_var,
            ValueBindingDeclaration::ordinary(
                ReceiverFnValueOrigin::DispatchProperty {
                    owner,
                    receiver_identity,
                    declared_ty: ty,
                    enum_entry_property: Some(property),
                    owner_storage: false,
                },
                ErrorProvenance::None,
            ),
        );
        Ok(())
    }

    /// An entry initializer's storage binding owns its live class rung. A recreated dispatch
    /// binding already retains that exact rung. Neither a nearer same-typed receiver nor a local
    /// with the same spelling can supply the declared member.
    fn declared_enum_property_input(
        &self,
        scope: &CheckerScope<'_>,
        storage: EnumEntryStorage,
    ) -> Option<DeclaredPropertyInput> {
        if self.active_declarations.is_some() && storage.property.owner.is_none() {
            return None;
        }
        for rung in scope.ancestors() {
            let mut selected = None;
            let mut conflict = false;
            rung.own_bindings(Ns::Value, |_, binding| {
                let Some(local) = binding.value() else { return };
                let input = match local.origin {
                    ReceiverFnValueOrigin::EnumEntryPropertyStorage {
                        owner,
                        field,
                        declaration,
                    } if owner == storage.owner && field == storage.property.sibling => rung
                        .innermost_class_receiver_identity()
                        .map(|receiver| DeclaredPropertyInput {
                            property: EnumEntryProperty {
                                owner: declaration,
                                sibling: field,
                            },
                            receiver,
                            is_var: local.is_var,
                        }),
                    ReceiverFnValueOrigin::DispatchProperty {
                        owner,
                        receiver_identity,
                        enum_entry_property: Some(property),
                        ..
                    } if owner == storage.owner && property.sibling == storage.property.sibling => {
                        Some(DeclaredPropertyInput {
                            property,
                            receiver: receiver_identity,
                            is_var: local.is_var,
                        })
                    }
                    _ => None,
                };
                if let Some(input) = input {
                    if input.property != storage.property {
                        conflict = true;
                    }
                    if selected.is_some_and(|previous: DeclaredPropertyInput| {
                        previous.property != input.property
                            || previous.receiver != input.receiver
                            || previous.is_var != input.is_var
                    }) {
                        conflict = true;
                    }
                    selected = Some(input);
                }
            });
            if conflict {
                return None;
            }
            if selected.is_some() {
                return selected;
            }
        }
        None
    }

    /// The selected source property and its exact live receiver are one input for reads and writes.
    pub(super) fn selected_enum_entry_receiver(
        &self,
        scope: &CheckerScope<'_>,
        local: Local,
    ) -> Option<(EnumEntryStorage, ImplicitReceiver)> {
        let storage = local.enum_entry_storage()?;
        let selected = self.declared_enum_property_input(scope, storage)?;
        let receiver = self.enum_property_read_receiver(
            scope,
            storage.owner,
            storage.property,
            selected.receiver,
        )?;
        Some((storage, receiver))
    }

    /// Qualify a declared property before placing its source input in a capture channel.
    pub(super) fn enum_entry_storage_has_declaring_receiver(
        &self,
        scope: &CheckerScope<'_>,
        local: Local,
    ) -> bool {
        let Some(storage) = local.enum_entry_storage() else {
            return false;
        };
        let Some(selected) = self.declared_enum_property_input(scope, storage) else {
            return false;
        };
        self.enum_property_read_receiver(scope, storage.owner, selected.property, selected.receiver)
            .is_some()
    }

    pub(super) fn declare_captured_enum_property(
        &mut self,
        scope: &CheckerScope<'_>,
        capture: &AnonymousObjectCapture,
    ) -> bool {
        let Some(storage) = capture.enum_entry_storage else {
            return false;
        };
        let Some(selected) = self.declared_enum_property_input(scope, storage) else {
            return false;
        };
        let Some(receiver) = self.enum_property_read_receiver(
            scope,
            storage.owner,
            storage.property,
            selected.receiver,
        ) else {
            return false;
        };
        self.declare_enum_entry_dispatch_property(
            scope,
            &capture.name,
            capture.ty,
            selected.is_var,
            EnumEntryPropertyBinding {
                owner: storage.owner,
                property: storage.property,
                receiver: Some(receiver.identity),
            },
        )
        .is_ok()
    }

    /// Validate the complete selected property/receiver pair before changing any read or use fact.
    fn enum_property_read_receiver(
        &self,
        scope: &CheckerScope<'_>,
        owner: TypeName,
        property: EnumEntryProperty,
        receiver: (usize, usize),
    ) -> Option<ImplicitReceiver> {
        let selected =
            self.declared_enum_property_input(scope, EnumEntryStorage { owner, property })?;
        if selected.property != property || selected.receiver != receiver {
            return None;
        }
        if let Some(index) = self.resolved_index {
            let declaration = property.owner?;
            if index.declaration_anchor(declaration)?.kind != crate::fir::DeclarationKind::EnumEntry
                || index
                    .owned_declaration(
                        declaration,
                        crate::fir::DeclarationKind::Property,
                        property.sibling,
                    )
                    .is_none()
            {
                return None;
            }
        }
        let mut live = self
            .implicit_receivers(scope)
            .into_iter()
            .filter(|candidate| candidate.identity == receiver && candidate.class_receiver);
        let selected = live.next()?;
        live.next().is_none().then_some(selected)
    }

    /// Entry initialization reads its own storage. A nested class's read of that same binding
    /// must keep the declared member and selected entry instance even after value captures retire.
    pub(super) fn publish_enum_entry_storage_read(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        local: Local,
    ) -> bool {
        let ReceiverFnValueOrigin::EnumEntryPropertyStorage {
            owner,
            field,
            declaration,
        } = local.origin
        else {
            return false;
        };
        let property = EnumEntryProperty {
            owner: declaration,
            sibling: field,
        };
        let Some(selected) =
            self.declared_enum_property_input(scope, EnumEntryStorage { owner, property })
        else {
            return false;
        };
        if self
            .enum_property_read_receiver(scope, owner, property, selected.receiver)
            .is_none()
        {
            return false;
        }
        if scope.innermost_class_receiver_identity() == Some(selected.receiver) {
            self.expr_lowers
                .insert(expression, ExprLowering::ClassStorageRead { field });
            self.read_flow_roots
                .insert(expression, scope::PathRoot::Value(local.flow_identity));
            self.anonymous_super_read_provenance.insert(
                expression,
                AnonymousSuperReadProvenance::LexicalBinding(local.flow_identity),
            );
            true
        } else {
            self.publish_enum_entry_property_read(
                scope,
                expression,
                owner,
                property,
                selected.receiver,
            )
        }
    }

    pub(super) fn publish_enum_entry_property_read(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        owner: TypeName,
        property: EnumEntryProperty,
        receiver: (usize, usize),
    ) -> bool {
        let Some(selected) = self.enum_property_read_receiver(scope, owner, property, receiver)
        else {
            return false;
        };
        self.read_flow_roots.remove(&expression);
        self.expr_lowers
            .insert(expression, ExprLowering::EnumEntryPropertyRead { property });
        self.mark_implicit_receiver_selection(expression, selected);
        true
    }
}

#[cfg(test)]
mod tests;
