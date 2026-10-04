//! Java's nullability at the classfile provider boundary.
//!
//! A Java type with no nullability qualifier is flexible (`T!`); `@NotNull`/`@Nullable` fix it.
//! A Java result additionally records how kotlinc's signature enhancement may treat it
//! ([`ResultEnhancement`]).

use super::super::classreader::JavaNullability;
use crate::libraries::ResultEnhancement;
use crate::types::{Ty, TypeName};

pub(super) fn java_type_nullability(ty: Ty, nullability: Option<JavaNullability>) -> Ty {
    if !ty.is_reference() {
        return ty;
    }
    // With no type-use qualifier, Java's flexibility applies recursively: `String[]` exposes
    // `Array<String!>!`, and `List<String>` exposes `List<String!>!`. Qualifying only the outer
    // classifier incorrectly rejects `null` as an expanded Java `String...` element.
    let ty = match ty.non_null() {
        Ty::Obj(name, arguments) if !arguments.is_empty() => {
            let arguments = arguments
                .iter()
                .map(|argument| java_type_argument_nullability(*argument))
                .collect::<Vec<_>>();
            // Retain the invariant lower bound of Java's flexible array projection. Common type
            // semantics derives `Array<out T>` as the upper bound of the surrounding platform type.
            Ty::obj_args_name(name, &arguments)
        }
        _ => ty,
    };
    // Java wrapper classes are Kotlin primitive types with Java's flexible/nullability qualifier.
    // Keep the physical wrapper in the JVM descriptor; the semantic signature must be `Int!`, not
    // `java.lang.Integer!`, so core type checking needs no representation-specific compatibility rule.
    let ty = ty
        .non_null()
        .obj_internal()
        .and_then(super::super::jvm_class_map::wrapper_to_kotlin_prim_name)
        .map(super::super::classpath::kotlin_name_to_ty)
        .unwrap_or(ty);
    match nullability {
        Some(JavaNullability::NotNull) => ty.non_null(),
        Some(JavaNullability::Nullable) => Ty::nullable(ty),
        None => Ty::platform_nullable(ty),
    }
}

/// Apply Java's unqualified flexibility inside a generic argument without discarding its semantic
/// wrapper. A declaration variable stays a variable whose bound is flexible; use-site variance stays
/// a projection whose interior is flexible.
pub(super) fn java_type_argument_nullability(ty: Ty) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => Ty::ty_param(name, java_type_nullability(*bound, None)),
        Ty::InProjection(inner) => Ty::in_projection(java_type_argument_nullability(*inner)),
        Ty::OutProjection(inner) => Ty::out_projection(java_type_argument_nullability(*inner)),
        Ty::StarProjection(inner) => Ty::star_projection(java_type_argument_nullability(*inner)),
        _ => java_type_nullability(ty, None),
    }
}

/// The enhancement a Java method's result starts from, before any overridden declaration is known.
///
/// `@NotNull` enhances it outright. A method of a JDK class that realizes a mapped Kotlin builtin
/// (`java.lang.Throwable` for `kotlin.Throwable`) is not a Java declaration in Kotlin's scope:
/// kotlinc's `JvmMappedScope` shows the builtin declaration it overrides instead, so an override
/// gives it that declaration's result without marking it enhanced.
pub(super) fn java_result_enhancement(
    owner: TypeName,
    nullability: Option<JavaNullability>,
) -> ResultEnhancement {
    if nullability == Some(JavaNullability::NotNull) {
        ResultEnhancement::NotNull
    } else if super::super::jvm_class_map::maps_to_distinct_jvm_internal(owner) {
        ResultEnhancement::MappedRealization
    } else {
        ResultEnhancement::None
    }
}
