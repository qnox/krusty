//! A value-class type in a method `Signature` position, mapped as kotlinc's `AbstractTypeMapper`
//! maps it: through `computeExpandedTypeForInlineClass` to the type the method actually carries.
//!
//! The expansion substitutes the type's arguments into the declared underlying type (`G<String>`
//! over `Comparable<T>` signs `Comparable<String>`), except that an underlying type parameter, or an
//! array of one, becomes its upper bound (`Arr<Int>` over `Array<T : Int>` signs `[Integer`). It
//! continues through nested value classes. A nullable value class keeps its box when the expansion
//! is a primitive or itself nullable, and otherwise makes the expansion nullable.
//!
//! The wildcard mode follows `getOptimalModeForSignaturePart`: a value class without type arguments
//! is mapped in `TypeMappingMode.DEFAULT`, whose arguments write every declaration-site wildcard
//! (`ICmp` over `Comparable<Int>` signs `Comparable<-Integer>` even as a return), while a generic
//! one takes the position's mode over its expansion.

use super::{JvmSignatureFormatter, Wildcards};
use crate::types::Ty;
use std::collections::{HashMap, HashSet};

/// One classifier visited while expanding, so a cyclic chain stops as kotlinc's does.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Visited {
    Parameter(&'static str),
    Class(crate::types::TypeName),
    Unsigned(Ty),
}

impl JvmSignatureFormatter<'_> {
    /// The expansion of a top-level value-class `ty` and the mode its signature element takes, or
    /// `None` when `ty` is not a value class the expansion changes.
    pub(super) fn value_class_position(
        &self,
        ty: Ty,
        wildcards: Wildcards,
    ) -> Option<(Ty, Wildcards)> {
        let semantic = ty.non_null();
        if !matches!(semantic, Ty::Obj(..)) || self.underlying(semantic).is_none() {
            return None;
        }
        let expanded = self.expanded(ty, &mut HashSet::new())?;
        if expanded == ty {
            return None;
        }
        let mode = if semantic.type_args().is_empty() {
            Wildcards::Generic
        } else {
            wildcards
        };
        Some((expanded, mode))
    }

    /// `computeExpandedTypeInner`.
    fn expanded(&self, ty: Ty, visited: &mut HashSet<Visited>) -> Option<Ty> {
        let semantic = ty.non_null();
        match semantic {
            Ty::TyParam(name, bound) => {
                if !visited.insert(Visited::Parameter(name)) {
                    return None;
                }
                let upper = *bound;
                let expanded = self.expanded(upper, visited)?;
                let upper_is_primitive_or_value =
                    is_primitive(upper) || self.underlying(upper.non_null()).is_some();
                Some(
                    if is_primitive(expanded) && is_nullable_type(ty) && upper_is_primitive_or_value
                    {
                        Ty::nullable(upper)
                    } else if is_nullable_type(expanded) || !ty.is_nullable() {
                        expanded
                    } else {
                        Ty::nullable(expanded)
                    },
                )
            }
            _ => {
                let Some(underlying) = self.underlying(semantic) else {
                    return Some(ty);
                };
                let key = match semantic {
                    Ty::Obj(classifier, _) => Visited::Class(classifier),
                    unsigned => Visited::Unsigned(unsigned),
                };
                if !visited.insert(key) {
                    return None;
                }
                let expanded = self.expanded(underlying, visited)?;
                Some(if !is_nullable_type(ty) {
                    expanded
                } else if is_nullable_type(expanded) || is_primitive(expanded) {
                    ty
                } else {
                    Ty::nullable(expanded)
                })
            }
        }
    }

    /// `getSubstitutedUnderlyingType` of a non-null value-class type: its declared underlying type
    /// with the type's arguments substituted. `None` when `ty` is no value class.
    fn underlying(&self, ty: Ty) -> Option<Ty> {
        if ty.is_unsigned() {
            return ty.scalar_value_repr();
        }
        let Ty::Obj(classifier, arguments) = ty else {
            return None;
        };
        // A dependency's declaration publishes its underlying type with its own type parameters
        // already erased to their bounds, so only this file's classes have arguments to substitute.
        let declared = match self.current_class(classifier) {
            Some(class) => class.fields.first().filter(|_| class.is_value)?.ty,
            None => *self.ir.external_value_class_name(classifier)?,
        };
        // An argument's type, or the parameter's upper bound for a star.
        let bindings = self
            .ir
            .class_signature_name(classifier)
            .map(|signature| {
                signature
                    .type_params
                    .iter()
                    .zip(arguments.iter())
                    .map(|(parameter, argument)| {
                        let argument = match argument {
                            Ty::StarProjection(_) => parameter.upper_bound(),
                            argument => argument.projection_inner().unwrap_or(*argument),
                        };
                        (parameter.semantic_name.clone(), argument)
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        Some(match type_parameter_or_array_thereof(declared) {
            Some(parameter) => {
                substitute_upper_bound(declared, parameter.substitute_erased(&bindings))
            }
            None => declared.substitute_erased(&bindings),
        })
    }
}

/// The upper bound of the type parameter `ty` is, or is an array (of arrays) of.
fn type_parameter_or_array_thereof(ty: Ty) -> Option<Ty> {
    match ty.non_null() {
        Ty::TyParam(_, bound) => Some(*bound),
        array if array.is_reference_array() => {
            type_parameter_or_array_thereof(array.array_elem()?.projection_read_ty())
        }
        _ => None,
    }
}

/// `substituteUpperBound`: the type parameter in `ty` replaced by `upper`, keeping each array and
/// `?` around it. An `in`-projected element can only be read as `Any?`.
fn substitute_upper_bound(ty: Ty, upper: Ty) -> Ty {
    let replaced = match ty.non_null() {
        Ty::TyParam(..) => upper,
        array => {
            let element = match array.array_elem().expect("an array of a type parameter") {
                Ty::InProjection(_) => Ty::nullable(Ty::obj("kotlin/Any")),
                element => substitute_upper_bound(element.projection_read_ty(), upper),
            };
            Ty::obj_args_name(crate::types::wk::array(), &[element])
        }
    };
    if is_nullable_type(ty) {
        Ty::nullable(replaced)
    } else {
        replaced
    }
}

/// kotlinc's `isPrimitiveType`: a non-null built-in primitive.
fn is_primitive(ty: Ty) -> bool {
    ty.is_jvm_scalar() && !ty.is_unsigned()
}

/// kotlinc's `isNullableType`: marked nullable, or a type parameter whose bound admits `null`.
fn is_nullable_type(ty: Ty) -> bool {
    ty.admits_null() || ty.upper_bound_admits_null()
}
