//! The receiver-less values one import rung offers for a name: the readable top-level properties
//! its records declare, the static properties of the classifiers it imports, and those
//! classifiers' enum entries. kotlinc collects every value of the nearest rung that has one and
//! reports a tie between them (`import first.KtNodeTypes.*` with `import second.SomeEnum.*`).

use super::scope_level_callables::{level_properties, symbols_at_scope_level, FunctionScopeRef};
use super::SymbolResolver;
use crate::libraries::{PropKind, PropertyInfo};
use crate::symbol_source::{SymbolNamespace, SymbolSource as _};
use crate::types::TypeName;

/// One value an import rung offers for a name.
#[derive(Clone)]
pub(crate) enum ScopedValue {
    Property(Box<PropertyInfo>),
    EnumEntry { owner: TypeName, name: String },
}

impl SymbolResolver<'_> {
    /// The values of the nearest import rung that offers any for `name`, in import order: the
    /// explicit imports, the current package, the explicit star imports, then the default imports.
    pub(crate) fn nearest_scope_level_values(&self, name: &str) -> Vec<ScopedValue> {
        let Some(FunctionScopeRef::Imports(imports)) = self.fn_scope else {
            return Vec::new();
        };
        let explicit = imports.explicit_targets(name);
        let explicit = self.owner_values(
            explicit
                .iter()
                .map(|(owner, declared)| (*owner, &**declared)),
        );
        if !explicit.is_empty() {
            return explicit;
        }
        for (index, level) in imports.levels().iter().enumerate() {
            let star_owners =
                index == super::scope_level_callables::EXPLICIT_STAR_IMPORT_RANK as usize;
            let owners = level.iter().map(|&owner| {
                let namespace = if star_owners && self.src.classifier(owner).is_some() {
                    SymbolNamespace::Classifier(owner)
                } else {
                    SymbolNamespace::Package(owner)
                };
                (namespace, name)
            });
            let values = self.owner_values(owners);
            if !values.is_empty() {
                return values;
            }
        }
        Vec::new()
    }

    /// The values each imported owner declares under its declared name, in owner order.
    fn owner_values<'n>(
        &self,
        owners: impl Iterator<Item = (SymbolNamespace, &'n str)>,
    ) -> Vec<ScopedValue> {
        let mut values = Vec::new();
        for (namespace, declared) in owners {
            let (owner, classifier_owner) = match namespace {
                SymbolNamespace::Classifier(owner) => (owner, true),
                SymbolNamespace::Package(owner) => (owner, false),
            };
            if classifier_owner
                && self
                    .src
                    .classifier(owner)
                    .is_some_and(|classifier| classifier.is_enum_entry(declared))
            {
                // A classpath enum also exposes its entry as a static field: the same declaration.
                values.push(ScopedValue::EnumEntry {
                    owner,
                    name: declared.to_owned(),
                });
                continue;
            }
            let records = symbols_at_scope_level(&self.src, declared, &[owner], classifier_owner);
            values.extend(
                level_properties(&records)
                    .filter(|property| property.kind == PropKind::TopLevel)
                    .filter(|property| property.readable() && self.property_visible(property))
                    .map(|property| ScopedValue::Property(Box::new(property.clone()))),
            );
        }
        values
    }
}
