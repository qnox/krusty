use super::{Checker, CheckerScope, ExprLowering, ResolvedEnumEntry};
use crate::ast::{ClassDecl, ExprId};
use crate::types::{Ty, TypeName};

/// The name of `Enum.entries`, the synthetic classifier property an enum entry can clash with.
const ENTRIES: &str = "entries";

/// What `entries` on an enum that also declares an entry `entries` names (kotlinc's
/// `DiscriminateSyntheticAndForbiddenProperties`): with `ForbidEnumEntryNamedEntries` the entry
/// resolves with low priority, so the property wins; without it the two are equal candidates under
/// `PrioritizedEnumEntries`, and the entry wins without either.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EntriesClash {
    Entry,
    Property,
    Ambiguous,
}

impl Checker<'_> {
    /// kotlinc's enum-entry declaration checks for `class`: a repeated entry name, and an entry
    /// named `entries` (`DECLARATION_OF_ENUM_ENTRY_ENTRIES`, a deprecation of
    /// `ForbidEnumEntryNamedEntries`) at the entry's name.
    pub(super) fn check_enum_entry_declarations(&mut self, class: &ClassDecl) {
        let mut seen = std::collections::HashSet::new();
        for entry in &class.enum_entries {
            if !seen.insert(entry.name.as_str()) {
                self.diags.error(
                    class.span,
                    format!(
                        "conflicting declaration: enum entry '{}' is declared more than once",
                        entry.name
                    ),
                );
            }
        }
        let gate = &self.file.language_gates.enum_entry_named_entries;
        for entry in class
            .enum_entries
            .iter()
            .filter(|entry| entry.name == ENTRIES)
        {
            let message = gate.message(
                "conflicting declarations: the enum entry 'entries' and the property \
                 'Enum.entries' (KT-48872).",
            );
            if gate.is_error() {
                self.diags.error(entry.span, message);
            } else {
                self.diags.warning(entry.span, message);
            }
        }
    }

    /// How `name` on the enum `owner` resolves when it is `entries` and the enum declares an entry
    /// of that name; `None` for every other name and classifier.
    pub(super) fn entries_clash(&self, owner: TypeName, name: &str) -> Option<EntriesClash> {
        let classifier = self.resolved_type_name(owner)?;
        if name != ENTRIES || !classifier.is_enum() || !classifier.is_enum_entry(name) {
            return None;
        }
        Some(
            if self.file.language_gates.enum_entry_named_entries.is_error() {
                EntriesClash::Property
            } else if self.file.prioritized_enum_entries {
                EntriesClash::Ambiguous
            } else {
                EntriesClash::Entry
            },
        )
    }

    /// Report `entries` at `reference` as ambiguous between the entry and `Enum.entries` of
    /// `owner` when [`Self::entries_clash`] says so. Returns whether it reported.
    pub(super) fn report_ambiguous_entries(
        &mut self,
        reference: ExprId,
        owner: TypeName,
        name: &str,
    ) -> bool {
        if self.entries_clash(owner, name) != Some(EntriesClash::Ambiguous) {
            return false;
        }
        let Some(property) = self
            .resolved_type_name(owner)
            .and_then(|classifier| classifier.classifier_property(owner, name))
        else {
            return false;
        };
        let enum_name = self.diagnostic_type_name(Ty::obj_name(owner), &[]);
        let property_ty = self.diagnostic_type_name(property.ty, &[]);
        self.diags.error(
            self.member_name_span(reference, name),
            format!(
                "overload resolution ambiguity between candidates:\n\
                 enum entry {name}: {enum_name}\n\
                 companion val {name}: {property_ty}"
            ),
        );
        true
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
        if self.report_ambiguous_entries(expression, owner, name) {
            return Some(Ty::Error);
        }
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
