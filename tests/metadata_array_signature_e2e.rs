//! `JvmMethodSignature.desc` (`@Metadata` `Function` extension field 100) for a signature holding a
//! `kotlin/Array`.
//!
//! A reader recovers a function's JVM descriptor by mapping each recorded `Type`'s CLASS NAME
//! through a flat table (`kotlin/Int` → `I`, `kotlin/collections/List` → `Ljava/util/List;`,
//! `kotlin/IntArray` → `[I`). `kotlin/Array` has no entry there: its descriptor depends on the type
//! ARGUMENT (`Array<String>` → `[Ljava/lang/String;`), which a name-keyed table cannot express. So
//! kotlinc records the descriptor explicitly for any signature holding one — and records none for
//! the specialized primitive arrays the table does cover.
//!
//! This is the rule, not "varargs need a descriptor": a `vararg` of a reference element needs one
//! only because it is RECORDED as an `Array`, and a `vararg xs: Int` (an `IntArray`) needs none.

use super::common;

/// Assert one same-module fixture is byte-identical to kotlinc's output for `class_internal`.
///
/// The stdlib is on the classpath because kotlinc's always is — without it krusty resolves `Array`,
/// `arrayOf` and every other stdlib name differently, and the comparison measures the missing
/// classpath rather than the metadata.
fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let Some(result) =
        common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
    else {
        eprintln!("skip ({stem}: provisioned kotlinc unavailable)");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// A plain `Array<T>` PARAMETER — no `vararg` anywhere. The descriptor is unrecoverable for exactly
/// the same reason a vararg's is, so kotlinc records it here too.
#[test]
fn an_array_parameter_records_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Payload(val v: Int)\n\
        \n\
        fun take(xs: Array<Payload>): Int = xs.size\n";
    assert_identical("ArrParam", SRC, "app/ArrParamKt");
}

/// An `Array<T>` RETURN. The rule scans every signature position, not just the parameters.
#[test]
fn an_array_return_records_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        fun make(n: Int): Array<String> = arrayOf()\n";
    assert_identical("ArrRet", SRC, "app/ArrRetKt");
}

/// An `Array<T>` EXTENSION RECEIVER, which the metadata separates from the value parameters but the
/// JVM descriptor does not.
#[test]
fn an_array_receiver_records_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        fun Array<String>.count2(): Int = size\n";
    assert_identical("ArrRecv", SRC, "app/ArrRecvKt");
}

/// A NULLABLE array is still an array — the `?` must not hide the classifier from the rule.
#[test]
fn a_nullable_array_parameter_records_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        fun maybe(xs: Array<String>?): Int = xs?.size ?: 0\n";
    assert_identical("ArrNull", SRC, "app/ArrNullKt");
}

/// A `vararg` of a REFERENCE element: recorded as `Array<out E>`, so it needs the descriptor — and
/// the `out` projection on the recorded array's argument, which source never wrote.
#[test]
fn a_reference_vararg_records_a_projected_array_and_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Payload(val v: Int)\n\
        \n\
        fun many(vararg xs: Payload): Int = xs.size\n";
    assert_identical("VarargRef", SRC, "app/VarargRefKt");
}

/// A PROPERTY's backing field takes the rule through `JvmPropertySignature.field`: an `Array`
/// field's descriptor depends on its type argument, so kotlinc records `JvmFieldSignature.desc`
/// (name omitted — that stays derivable) and records nothing for the `IntArray` its table maps.
#[test]
fn an_array_property_records_its_field_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        val xs: Array<String> = arrayOf()\n\
        \n\
        val ns: IntArray = intArrayOf()\n";
    assert_identical("ArrProp", SRC, "app/ArrPropKt");
}

/// A `vararg` of a PRIMITIVE element is an `IntArray`, which the reader's table maps on its own —
/// kotlinc records NO descriptor, and neither may krusty. The boundary case that keeps the rule
/// honest: a vararg-keyed rule would over-record here.
#[test]
fn a_primitive_vararg_records_no_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        fun sum(vararg xs: Int): Int = xs.size\n";
    assert_identical("VarargPrim", SRC, "app/VarargPrimKt");
}

/// A declared `IntArray` parameter, for the same reason, and a `List<Array<String>>` — an array
/// NESTED inside another type is erased away before the descriptor, so the outer `List` maps
/// cleanly and nothing is recorded.
#[test]
fn a_primitive_array_and_a_nested_array_record_no_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        fun ints(xs: IntArray): Int = xs.size\n\
        \n\
        fun nested(xs: List<Array<String>>): Int = xs.size\n";
    assert_identical("ArrNested", SRC, "app/ArrNestedKt");
}
