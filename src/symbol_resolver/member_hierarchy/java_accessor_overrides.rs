//! A Java method that overrides a Kotlin property's accessor is that property's accessor, not a
//! function of the Java classifier. kotlinc's Java scope leaves it out of the classifier's
//! functions: `getA()` in a Java class extending a Kotlin `open val a` is neither callable as a
//! function of that class nor an implementation of another supertype's `getA()`, and a Kotlin
//! `override fun getA()` below it overrides only the functions that remain.

use crate::libraries::{FunctionInfo, LibraryCallable, LibraryType};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// Whether `function`, declared by the Java `classifier` applied as `current`, implements the
/// accessor of a Kotlin property one of the classifier's supertypes declares or inherits.
pub(super) fn overrides_kotlin_property_accessor(
    source: &dyn SymbolSource,
    classifier: &LibraryType,
    current: Ty,
    function: &FunctionInfo,
) -> bool {
    if classifier.is_kotlin
        || function.classifier_static
        || function.kind != crate::libraries::FnKind::Member
    {
        return false;
    }
    let name = physical_name(&function.callable);
    let (properties, setter) = match function.callable.params.len() {
        0 => (
            crate::names::accessor_property_name(name)
                .into_iter()
                .collect(),
            false,
        ),
        1 => (crate::names::setter_property_names(name), true),
        _ => return false,
    };
    if properties.is_empty() {
        return false;
    }
    let supertypes = super::projected_supertypes(source, classifier, current);
    properties.iter().any(|property| {
        supertypes.iter().any(|supertype| {
            super::member_declarations_in_hierarchy(source, *supertype, property)
                .into_parts()
                .1
                .overloads
                .iter()
                .filter(|declared| {
                    declared.kind == crate::libraries::PropKind::Member
                        && declares_kotlin(source, declared)
                })
                .any(|declared| {
                    let accessor = if setter {
                        declared.setter.as_deref()
                    } else {
                        Some(&declared.getter)
                    };
                    accessor.is_some_and(|accessor| physical_name(accessor) == name)
                })
        })
    })
}

fn physical_name(callable: &LibraryCallable) -> &str {
    callable.physical_name.as_deref().unwrap_or(&callable.name)
}

fn declares_kotlin(source: &dyn SymbolSource, property: &crate::libraries::PropertyInfo) -> bool {
    let owner = property
        .getter
        .declaration_owner
        .map_or(property.getter.owner, |owner| owner.name());
    source
        .classifier(owner)
        .is_some_and(|classifier| classifier.is_kotlin)
}
