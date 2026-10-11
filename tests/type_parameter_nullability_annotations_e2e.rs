//! kotlinc's `@NotNull`/`@Nullable` on a position declared by a type parameter, compared as whole
//! classes against the reference compiler.
//!
//! kotlinc's `AnnotationCodegen` reads the declared type of the occurrence: a bare type parameter
//! whose upper-bound chain admits null is left unannotated, a marked `T?` is `@Nullable`, and a
//! non-null-bounded `T : Cargo` or a definitely-non-null `T & Any` is `@NotNull`. The same rule
//! covers fields, getters, setters, constructor and function parameters, results, and a value
//! class's `-impl` statics. A value class's `constructor-impl` returns the class type itself, so
//! its result is `@NotNull` whatever the carrier; `box-impl` and `unbox-impl` carry nothing.
//!
//! Each class's header interns its annotation descriptors, so the classification also fixes the
//! constant-pool order: the comparison is byte-for-byte.

use super::common;

const SRC: &str = "package app\n\
    open class Cargo\n\
    @JvmInline value class Bounded<T : Cargo>(val cargo: T) {\n\
    \x20   fun same(): T = cargo\n\
    \x20   fun maybe(other: T?): T? = other\n\
    }\n\
    @JvmInline value class NullableBounded<T : Cargo?>(val cargo: T) {\n\
    \x20   fun same(): T = cargo\n\
    \x20   fun maybe(other: T?): T? = other\n\
    }\n\
    @JvmInline value class Marked<T : Cargo>(val cargo: T?)\n\
    @JvmInline value class Unbounded<T>(val cargo: T)\n\
    class Holder<T : Cargo>(val cargo: T, var spare: T?) {\n\
    \x20   fun take(value: T): T = value\n\
    }\n\
    class NullableHolder<T : Cargo?>(val cargo: T, var spare: T?) {\n\
    \x20   fun take(value: T): T = value\n\
    }\n\
    class Chained<A : Cargo?, B : A>(val cargo: B)\n";

/// `T & Any` positions. Their Kotlin metadata differs from kotlinc's in the definitely-non-null
/// version requirement, which is outside this rule, so these classes are compared as whole `javap -v`
/// listings with only the metadata `d1` payload left out.
const DEFINITELY_NON_NULL_SRC: &str = "package app\n\
    @JvmInline value class Present<T>(val cargo: T & Any)\n\
    class PresentHolder<T>(val cargo: T & Any, var spare: T & Any, plain: T & Any) {\n\
    \x20   fun take(value: T & Any): T & Any = value\n\
    }\n";

fn compare(name: &str, src: &str, class: &str) -> common::ReferenceComparison {
    common::compare_with_kotlinc_plugin(name, src, class, &[common::stdlib_jar()], "1.8", &[])
        .expect("reference kotlinc is provisioned")
}

#[test]
fn type_parameter_positions_annotate_like_kotlinc() {
    for class in [
        "app/Bounded",
        "app/NullableBounded",
        "app/Marked",
        "app/Unbounded",
        "app/Holder",
        "app/NullableHolder",
        "app/Chained",
    ] {
        let built = compare("TypeParameterNullability", SRC, class);
        assert!(
            built.reference_bytes == built.krusty_bytes,
            "{class} differs from kotlinc\n--- kotlinc\n{}\n--- krusty\n{}",
            built.reference,
            built.krusty
        );
    }
}

/// The listing without its location header and the Kotlin metadata `d1` payload (an escaped
/// protobuf string, printed both as its constant and inside `@Metadata`).
fn without_metadata_payload(listing: &str) -> Vec<&str> {
    listing
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with("Classfile ")
                && !line.starts_with("Last modified")
                && !line.starts_with("SHA-256")
                && !line.contains("\\u0000")
        })
        .collect()
}

#[test]
fn definitely_non_null_type_parameter_positions_are_not_null() {
    for class in ["app/Present", "app/PresentHolder"] {
        let built = compare(
            "DefinitelyNonNullTypeParameterNullability",
            DEFINITELY_NON_NULL_SRC,
            class,
        );
        assert_eq!(
            without_metadata_payload(&built.krusty),
            without_metadata_payload(&built.reference),
            "{class}"
        );
    }
}
