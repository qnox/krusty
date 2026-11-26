//! Java methods that override every accessor of a Kotlin property are that property's override,
//! not functions of the Java classifier. kotlinc's Java scope leaves them out of the classifier's
//! functions: `getA()` in a Java class extending a Kotlin `open val a` is neither callable as a
//! function of that class nor an implementation of another supertype's `getA()`, and a Kotlin
//! `override fun getA()` below it overrides only the functions that remain. For a `var` the Java
//! class must override the getter and the setter; one of them alone stays an ordinary function.
//!
//! The Java provider records which properties a method's platform spelling names, and which
//! methods would be the same property's other accessor ([`PropertyAccessorRole`]). This walk
//! confirms the property against the supertypes' Kotlin declarations and each accessor against its
//! applied shape, so an overload such as `String setA(String)` beside `var a: Int` stays a function.
//! Only an inherited accessor is overridden: a superclass's `private val a` leaves the Java class's
//! `getA()` an ordinary function.

use crate::libraries::{
    FnKind, FunctionInfo, LibraryType, PropKind, PropertyAccessorRole, PropertyInfo,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// Whether `function`, declared by `classifier` applied as `current`, is part of the classifier's
/// override of a Kotlin property that one of its supertypes declares or inherits.
pub(super) fn overrides_kotlin_property_accessor(
    source: &dyn SymbolSource,
    classifier: &LibraryType,
    current: Ty,
    function: &FunctionInfo,
) -> bool {
    let Some(role) = function.callable.property_accessor_role.as_deref() else {
        return false;
    };
    if !is_instance_member(function) {
        return false;
    }
    let supertypes = super::projected_supertypes(source, classifier, current);
    role.candidates.iter().any(|candidate| {
        supertypes.iter().any(|supertype| {
            super::member_declarations_in_hierarchy(source, *supertype, &candidate.property)
                .into_parts()
                .1
                .overloads
                .iter()
                .filter(|property| {
                    property.kind == PropKind::Member
                        && property.context_count == 0
                        && super::member_is_inheritable(property.visibility)
                        && declares_kotlin(source, property)
                })
                .any(|property| {
                    accessor_shape_matches(source, function, role.setter, property)
                        && (property.setter.is_none()
                            || declares_counterpart(
                                source,
                                classifier,
                                current,
                                (role, &candidate.counterparts),
                                property,
                            ))
                })
        })
    })
}

fn is_instance_member(function: &FunctionInfo) -> bool {
    function.kind == FnKind::Member && !function.classifier_static
}

/// Whether `classifier` also declares the other accessor of the `var` `property`: the setter for a
/// getter, the getter for a setter.
fn declares_counterpart(
    source: &dyn SymbolSource,
    classifier: &LibraryType,
    current: Ty,
    (role, counterparts): (&PropertyAccessorRole, &[String]),
    property: &PropertyInfo,
) -> bool {
    counterparts.iter().any(|name| {
        super::declared_callables(source, classifier, current, name)
            .into_parts()
            .0
            .overloads
            .iter()
            .any(|other| {
                is_instance_member(other)
                    && other
                        .callable
                        .property_accessor_role
                        .as_deref()
                        .is_some_and(|other_role| other_role.setter != role.setter)
                    && accessor_shape_matches(source, other, !role.setter, property)
            })
    })
}

/// Whether `function` has the applied shape of `property`'s accessor, so that it overrides that
/// accessor rather than overloading it: a getter takes nothing and returns a subtype of the
/// property's type, and a setter takes exactly the property's type and returns `Unit`. A private
/// setter is not inherited, so nothing overrides it.
fn accessor_shape_matches(
    source: &dyn SymbolSource,
    function: &FunctionInfo,
    setter: bool,
    property: &PropertyInfo,
) -> bool {
    let result = function.ret.apply(function.callable.ret);
    if !setter {
        return function.callable.params.is_empty()
            && crate::symbol_resolver::resolution_subtype(source, result, property.ty);
    }
    let [parameter] = function.callable.params.as_slice() else {
        return false;
    };
    property.setter.is_some()
        && super::member_is_inheritable(property.setter_visibility)
        && result == Ty::Unit
        && crate::symbol_resolver::resolution_subtype(source, *parameter, property.ty)
        && crate::symbol_resolver::resolution_subtype(source, property.ty, *parameter)
}

fn declares_kotlin(source: &dyn SymbolSource, property: &PropertyInfo) -> bool {
    let owner = property
        .getter
        .declaration_owner
        .map_or(property.getter.owner, |owner| owner.name());
    source
        .classifier(owner)
        .is_some_and(|classifier| classifier.is_kotlin)
}
