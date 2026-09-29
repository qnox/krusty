use super::{Checker, CheckerScope, ResolvedEnumEntry};
use crate::ast::ExprId;
use crate::types::TypeName;

impl Checker<'_> {
    fn classifier_enum_entry_ordinal(&self, owner: TypeName, name: &str) -> Option<usize> {
        self.resolver()
            .classifier(owner)?
            .enum_entries
            .iter()
            .position(|entry| entry == name)
    }

    pub(super) fn classifier_has_enum_entry(&self, owner: TypeName, name: &str) -> bool {
        self.classifier_enum_entry_ordinal(owner, name).is_some()
    }

    /// The enum named by a simple qualifier whose value facet is that enum's companion.
    /// `Enum.entries` is a classifier property of the enum, not a member of the companion value.
    pub(super) fn enum_entries_value_classifier(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
    ) -> Option<TypeName> {
        let crate::ast::Expr::Name(name) = self.file.expr(receiver) else {
            return None;
        };
        let owner = self.select_classifier(scope, name).found()?;
        self.resolved_type_name(owner)
            .is_some_and(|classifier| classifier.is_enum())
            .then_some(owner)
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
