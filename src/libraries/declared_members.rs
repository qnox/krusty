//! A library classifier's own members as its source language declared them, beside its
//! [`super::DeclarationHeader`], for a consumer that shows declarations rather than resolving
//! against them: no inherited member, no member a platform class adds, and every type as declared.

pub use crate::metadata::semantic::KotlinModality;
use crate::types::{Ty, TyKind, TypeName, Visibility};

/// The members a library classifier declares, each list in declaration order.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredMembers {
    pub functions: Vec<DeclaredFunction>,
    pub properties: Vec<DeclaredProperty>,
    pub constructors: Vec<DeclaredConstructor>,
    /// The static properties: a Java class's static fields.
    pub static_properties: Vec<DeclaredProperty>,
    /// The classifiers declared in its body.
    pub nested: Vec<TypeName>,
}

/// A member function.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredFunction {
    pub name: String,
    pub parameters: Vec<DeclaredParameter>,
    pub ret: Ty,
    pub visibility: Visibility,
    pub modality: KotlinModality,
    pub is_operator: bool,
    pub is_infix: bool,
    /// Whether the declaration states annotations, which this view does not carry yet.
    pub annotated: bool,
}

/// A member property.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredProperty {
    pub name: String,
    pub ty: Ty,
    pub is_var: bool,
    pub visibility: Visibility,
    pub modality: KotlinModality,
    /// Kotlin reads it off a Java declaration that is not a property: an annotation element.
    pub from_java_method: bool,
    /// Whether the declaration states annotations, which this view does not carry yet.
    pub annotated: bool,
}

/// A constructor. A Java annotation class has one Kotlin reads off its elements.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredConstructor {
    pub parameters: Vec<DeclaredParameter>,
    pub visibility: Visibility,
}

/// A value parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclaredParameter {
    pub name: String,
    /// The declared type; a `vararg` parameter's element type.
    pub ty: Ty,
    pub is_vararg: bool,
    pub has_default: bool,
}

impl DeclaredParameter {
    /// A parameter whose declared type is `ty`; a `vararg` one takes the element `E` of its
    /// `Array<out E>` or primitive array type. `None` when a `vararg` type is not an array.
    pub fn new(name: String, ty: Ty, is_vararg: bool, has_default: bool) -> Option<Self> {
        let ty = match is_vararg {
            true => match ty.array_elem()?.kind() {
                TyKind::OutProjection(element) => element,
                _ => ty.array_elem()?,
            },
            false => ty,
        };
        Some(Self {
            name,
            ty,
            is_vararg,
            has_default,
        })
    }
}
