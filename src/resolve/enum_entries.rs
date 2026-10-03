use super::{Checker, CheckerScope, ExprLowering, ResolvedEnumEntry};
use crate::ast::ExprId;
use crate::types::{Ty, TypeName};

impl Checker<'_> {
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

    /// Enum entries and static members exported by a classifier star import (`import Game.*`) use
    /// the classifier as their import scope; treating every star path as a package loses this rung.
    pub(super) fn explicitly_imported_enum_entry(&self, name: &str) -> Option<(TypeName, String)> {
        let (namespace, declared_name) = self.function_import_scope.explicit_target(name)?;
        let crate::symbol_source::SymbolNamespace::Classifier(owner) = namespace else {
            return None;
        };
        let is_entry = self.classifier_has_enum_entry(owner, &declared_name);
        is_entry.then_some((owner, declared_name))
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

    pub(super) fn star_imported_enum_entry(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<TypeName> {
        let mut selected = None;
        for import in self
            .file
            .import_paths
            .iter()
            .filter(|import| import.wildcard)
        {
            let Some(owner) = self.select_classifier(scope, &import.path()).found() else {
                continue;
            };
            let is_entry = self.classifier_has_enum_entry(owner, name);
            if !is_entry {
                continue;
            }
            match selected {
                None => selected = Some(owner),
                Some(previous) if previous == owner => {}
                Some(_) => return None,
            }
        }
        selected
    }
}
