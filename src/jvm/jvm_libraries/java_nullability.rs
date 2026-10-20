//! Java's nullability at the classfile provider boundary.
//!
//! A Java type with no nullability qualifier is flexible (`T!`); `@NotNull`/`@Nullable` fix it.
//! A Java result additionally records how kotlinc's signature enhancement may treat it
//! ([`ResultEnhancement`]).

use super::super::JavaNullability;
use crate::libraries::ResultEnhancement;
use crate::types::{Ty, TypeName};

/// Normalize a Java field type. An enum constant (`ACC_ENUM`) is not-null: kotlinc reports
/// `Enum.ENTRY == null` as always false and emits no platform guard. Every other unqualified Java
/// reference field stays flexible, including a static field whose type is an enum but which is not
/// an enum constant.
pub(super) fn java_field_type(ty: Ty, access: u16, annotated: Option<JavaNullability>) -> Ty {
    let nullability = if access & super::super::classreader::ACC_ENUM != 0 {
        Some(JavaNullability::NotNull)
    } else {
        annotated
    };
    java_type_nullability(ty, nullability)
}

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
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| java_type_argument_nullability(*parameter))
                .collect(),
            java_type_argument_nullability(signature.ret),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        // A Java type variable's bound is a Java type, so it admits null (`E`'s implicit bound is
        // `Any!`). As a whole member type (`E get(int)`) the variable is flexible through the
        // platform wrapper below alone; its own bound is the upper face of that Java bound (`Any?`)
        // and does not repeat the wrapper's flexibility, which a bare variable's bound records
        // only where it is an argument (`List<E>`). The parse leaves a placeholder `Any` here, and
        // read as the bound of `E & Any` it made `ArrayList<V?>.get` substitute the non-null `V`.
        Ty::TyParam(name, bound) => Ty::ty_param(name.as_str(), Ty::nullable(bound.non_null())),
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
        // `@NotNull T` is `T & Any`: a nullable argument substituted for `T` stays non-null.
        Some(JavaNullability::NotNull) => ty.definitely_non_null(),
        Some(JavaNullability::Nullable) => Ty::nullable(ty),
        None => Ty::platform_nullable(ty),
    }
}

/// Apply Java's unqualified flexibility inside a generic argument without discarding its semantic
/// wrapper. A declaration variable keeps its stable symbolic identity while its bound records the
/// Java declaration's flexibility. Selected-call publication owns the occurrence-level `T!` fact;
/// putting that wrapper into the shared semantic type would leak a Java provider detail through
/// Kotlin collection supertypes and source-declared generic operations. Use-site variance stays a
/// projection whose interior is flexible.
pub(super) fn java_type_argument_nullability(ty: Ty) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => {
            Ty::ty_param(name.as_str(), java_type_nullability(*bound, None))
        }
        Ty::InProjection(inner) => Ty::in_projection(java_type_argument_nullability(*inner)),
        Ty::OutProjection(inner) => Ty::out_projection(java_type_argument_nullability(*inner)),
        Ty::StarProjection(inner) => Ty::star_projection(java_type_argument_nullability(*inner)),
        _ => java_type_nullability(ty, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_java_type_variable_member_type_is_flexible_with_a_bound_that_admits_null() {
        let any = Ty::obj("kotlin/Any");
        let number = Ty::obj("kotlin/Number");

        assert_eq!(
            java_type_nullability(Ty::ty_param("E", any), None),
            Ty::platform_nullable(Ty::ty_param("E", Ty::nullable(any)))
        );
        assert_eq!(
            java_type_nullability(Ty::ty_param("T", Ty::platform_nullable(number)), None),
            Ty::platform_nullable(Ty::ty_param("T", Ty::nullable(number)))
        );
        assert_eq!(
            java_type_nullability(Ty::ty_param("E", any), Some(JavaNullability::Nullable)),
            Ty::nullable(Ty::ty_param("E", Ty::nullable(any)))
        );
        assert_eq!(
            java_type_nullability(Ty::ty_param("E", any), Some(JavaNullability::NotNull)),
            Ty::ty_param("E", any),
            "`@NotNull E` stays `E & Any`"
        );
    }

    #[test]
    fn an_unqualified_java_type_variable_keeps_identity_and_a_flexible_bound() {
        let variable = Ty::ty_param("T", Ty::obj("java/lang/Object"));

        assert_eq!(
            java_type_argument_nullability(variable),
            Ty::ty_param("T", Ty::platform_nullable(Ty::obj("java/lang/Object")),)
        );
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
