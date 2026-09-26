//! Receiver-less properties associated with a classifier (`C.name`): a Java static field, a
//! companion `@JvmField`, or a `companion { … }` block property. Providers publish them in the
//! classifier's namespace record; these queries read them from that record and decide whether the
//! lexical access site sees them.

use crate::assignable::{is_subtype, TyCtx};
use crate::libraries::PropertyInfo;
use crate::symbol_source::{SymbolNamespace, SymbolSource as _};
use crate::types::{Ty, TypeName, Visibility};

use super::{SourceOracle, SymbolResolver};

impl SymbolResolver<'_> {
    /// The receiver-less property `internal.name` names: the one `internal`'s namespace record
    /// publishes as associated with it.
    pub fn associated_property(&self, internal: TypeName, name: &str) -> Option<PropertyInfo> {
        self.src
            .symbols(SymbolNamespace::Classifier(internal), name)
            .callables
            .properties()
            .iter()
            .find(|property| property.associated_classifier == Some(internal))
            .cloned()
    }

    /// Provider-normalized classifier property visible from this resolver's lexical access site.
    /// The declaration may be realized however the platform chooses; this operation deals only in
    /// Kotlin property shape and source visibility.
    pub(crate) fn accessible_classifier_associated_property(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<PropertyInfo> {
        self.associated_property(internal, name)
            .filter(|property| self.associated_property_accessible(property))
    }

    /// Whether an associated property's declaration is visible from this resolver's lexical
    /// access site. A current-module declaration is in this module; a dependency's `internal` one
    /// is visible only through the provider's friend-module rule, and its `private` one never.
    pub(crate) fn associated_property_accessible(&self, property: &PropertyInfo) -> bool {
        let current_module = property.source_key.is_some() || property.stable_declaration.is_some();
        match property.visibility {
            Visibility::Public => true,
            Visibility::Internal => {
                current_module
                    || self
                        .module
                        .is_some_and(|module| module.classifier(property.owner).is_some())
                    || self.lib.internal_accessible(property.owner)
            }
            Visibility::PackagePrivate => self.package_private_member_accessible(property.owner),
            Visibility::Private => {
                current_module
                    || self.lexical_classes.iter().copied().any(|enclosing| {
                        enclosing == property.owner
                            || std::iter::successors(enclosing.nested_owner(), |owner| {
                                owner.nested_owner()
                            })
                            .any(|owner| owner == property.owner)
                    })
            }
            Visibility::Protected => self.lexical_classes.iter().copied().any(|enclosing| {
                is_subtype(
                    &TyCtx::new(),
                    &SourceOracle(&self.src),
                    Ty::obj_name(enclosing),
                    Ty::obj_name(property.owner),
                )
            }),
        }
    }
}
