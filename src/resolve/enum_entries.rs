use super::{
    Checker, CheckerScope, ClassSig, ExprLowering, ResolvedConstructor, ResolvedCtorDelegation,
    ResolvedCtorDelegationTarget, ResolvedEnumEntry,
};
use crate::ast::ExprId;
use crate::symbol_resolver::ScopedValue;
use crate::types::{Ty, TypeName};

impl Checker<'_> {
    /// Finish the checked arguments of an enum `entry` and record the constructor it selects with
    /// its declaration-owned classifier identity.
    /// A deferred header does not erase the identity already bound for that same declaration.
    pub(super) fn record_enum_entry_constructor(
        &mut self,
        scope: &CheckerScope<'_>,
        entry: &crate::ast::AstEnumEntry,
        streamed_enum_owner: Option<crate::fir::DeclarationId>,
        enum_signature: Option<&ClassSig>,
        selected: ResolvedCtorDelegation,
    ) {
        for &argument in &entry.args {
            self.finish_executable_root(argument);
        }
        let (primary, stable_declaration) = match &selected.target {
            ResolvedCtorDelegationTarget::ThisPrimary { .. } => (
                true,
                streamed_enum_owner
                    .and_then(|owner| {
                        self.resolved_index?.owned_declaration(
                            owner,
                            crate::fir::DeclarationKind::Constructor,
                            0,
                        )
                    })
                    .or_else(|| {
                        enum_signature
                            .and_then(|signature| signature.primary_constructor_declaration)
                    }),
            ),
            ResolvedCtorDelegationTarget::ThisSecondary { index, .. } => {
                let sibling =
                    u32::try_from(*index + 1).expect("too many enum secondary constructors");
                (
                    false,
                    streamed_enum_owner
                        .and_then(|owner| {
                            self.resolved_index?.owned_declaration(
                                owner,
                                crate::fir::DeclarationKind::Constructor,
                                sibling,
                            )
                        })
                        .or_else(|| {
                            enum_signature.and_then(|signature| {
                                signature
                                    .secondary_constructor_declarations
                                    .get(*index)
                                    .copied()
                                    .flatten()
                            })
                        }),
                )
            }
            ResolvedCtorDelegationTarget::Super { .. } => {
                unreachable!("an enum entry selects a constructor of its enum class")
            }
        };
        // The entry's own annotations (`@OptIn(M::class) ENTRY`) accept what its call needs.
        let entry_policy_depth =
            self.push_declaration_policies(scope, &entry.annotations, &entry.annotation_args);
        self.check_enum_entry_opt_in(entry.span, entry.explicit_call, stable_declaration);
        self.active_lexical_policies.truncate(entry_policy_depth);
        let entry_span = entry.span;
        self.resolved_enum_entry_constructors.insert(
            (entry_span.lo, entry_span.hi),
            ResolvedConstructor::Source {
                // A deferred local enum publishes its classifier identity before its
                // header. The header remains authoritative when it exists; the identity
                // is the owner Pass 2 already bound for that same declaration.
                owner: streamed_enum_owner
                    .and_then(|owner| {
                        let index = self.resolved_index?;
                        index
                            .classifier_header(owner)
                            .map(|header| header.classifier)
                            .or_else(|| index.classifier_identity(owner))
                    })
                    .or_else(|| enum_signature.map(ClassSig::internal_name))
                    .expect("checked enum must have a semantic owner"),
                stable_declaration,
                outer: None,
                primary,
                context_args: selected.context_args,
                declaration_params: selected.target.params().to_vec(),
                params: selected.target.params().to_vec(),
                argument_slots: selected.argument_slots,
                argument_bindings: Vec::new(),
                argument_types: selected.argument_types,
                omitted: selected.omitted,
                vararg: selected.vararg,
                default_masks: selected.default_masks,
            },
        );
    }

    pub(super) fn classifier_enum_entry_ordinal(
        &self,
        owner: TypeName,
        name: &str,
    ) -> Option<usize> {
        self.resolver()
            .classifier(owner)?
            .enum_entries
            .iter()
            .position(|entry| entry == name)
    }

    pub(super) fn classifier_has_enum_entry(&self, owner: TypeName, name: &str) -> bool {
        self.classifier_enum_entry_ordinal(owner, name).is_some()
    }

    /// The enum whose companion is the already-bound receiver value. `Enum.entries` is a
    /// classifier property of the enum, not a member of the companion value. Inspecting the
    /// resolved receiver type preserves a local/property root that shadows the enum spelling.
    pub(super) fn enum_entries_value_classifier(
        &mut self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
    ) -> Option<TypeName> {
        let companion = self.expr(scope, receiver).non_null().obj_internal()?;
        let owner = companion.nested_owner()?;
        let declaration = self.resolver().classifier(owner)?;
        if !declaration.is_enum() {
            return None;
        }
        let declared_companion = declaration.companion_object.as_ref()?.1;
        (declared_companion == companion).then_some(owner)
    }

    pub(super) fn record_enum_entry(&mut self, expression: ExprId, owner: TypeName, name: &str) {
        let ordinal = self.classifier_enum_entry_ordinal(owner, name);
        if let Some(ordinal) = ordinal.and_then(|ordinal| u32::try_from(ordinal).ok()) {
            let declaration = self.resolved_index.and_then(|index| {
                let classifier = index.classifier_declaration(owner)?;
                index.owned_declaration(classifier, crate::fir::DeclarationKind::EnumEntry, ordinal)
            });
            self.resolved_enum_entries.insert(
                expression,
                ResolvedEnumEntry {
                    classifier: owner,
                    ordinal,
                    name: name.to_owned(),
                    declaration,
                },
            );
        }
    }

    /// Read `name` as the enum entry an import rung offers. The nearest rung offering a value for
    /// the name decides: one enum entry binds, and an entry tied with any other value of that rung
    /// (another enum's entry, a static field, a top-level property) is kotlinc's ambiguity. A rung
    /// offering only properties is left to property selection, which owns their context arguments.
    pub(super) fn read_imported_enum_entry(
        &mut self,
        expression: ExprId,
        name: &str,
    ) -> Option<Ty> {
        let mut values = self.resolver().nearest_scope_level_values(name);
        let mut seen = Vec::new();
        values.retain(|value| match value {
            ScopedValue::EnumEntry { owner, name } => {
                let fresh = !seen.contains(&(*owner, name.clone()));
                seen.push((*owner, name.clone()));
                fresh
            }
            ScopedValue::Property(_) => true,
        });
        if !values
            .iter()
            .any(|value| matches!(value, ScopedValue::EnumEntry { .. }))
        {
            return None;
        }
        if let [ScopedValue::EnumEntry { owner, name }] = values.as_slice() {
            let owner = *owner;
            self.record_enum_entry(expression, owner, &name.clone());
            return Some(Ty::obj_name(owner));
        }
        let mut message = "overload resolution ambiguity between candidates:".to_string();
        for value in &values {
            message.push('\n');
            message.push_str(&self.scoped_value_display(value));
        }
        self.diags.error(self.span(expression), message);
        Some(Ty::Error)
    }

    /// kotlinc's rendering of one candidate value: `enum entry SOME: SomeEnum`, a platform
    /// declaration's own label (`static field SOME: String`), or `val SOME: Int`. A candidate list
    /// renders a platform type by its lower bound.
    fn scoped_value_display(&self, value: &ScopedValue) -> String {
        match value {
            ScopedValue::EnumEntry { owner, name } => {
                let ty = Ty::obj_name(*owner);
                format!(
                    "enum entry {name}: {}",
                    self.diagnostic_type_name(ty, &[ty])
                )
            }
            ScopedValue::Property(property) => {
                let ty = property.ty.platform_lower_bound();
                property
                    .getter
                    .external_property_identity
                    .and_then(|identity| {
                        self.libraries.external_property_diagnostic_label(
                            identity,
                            &property.name,
                            ty,
                        )
                    })
                    .unwrap_or_else(|| {
                        let keyword = if property.setter.is_some() {
                            "var"
                        } else {
                            "val"
                        };
                        let ty = self.diagnostic_type_name(ty, &[ty]);
                        format!("{keyword} {}: {ty}", property.name)
                    })
            }
        }
    }

    /// The synthetic classifier property on the owner the scope tower already selected.
    ///
    /// The prioritized rung carries that owner. `name` is only the lookup spelling;
    /// [`crate::libraries::ImplicitClassifierProperty`] decides the result.
    pub(super) fn prioritized_classifier_property(
        &mut self,
        expression: ExprId,
        name: &str,
        owner: TypeName,
    ) -> Option<Ty> {
        let (owner, property) = self.classifier_property_for_owner(owner, name)?;
        let crate::libraries::ImplicitClassifierProperty::EnumEntries = property.operation;
        let ty = property.ty;
        self.expr_lowers.insert(
            expression,
            ExprLowering::ClassifierPropertyRead {
                owner,
                property: Box::new(property),
            },
        );
        Some(ty)
    }
}
