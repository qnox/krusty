//! The array rules of `metadata_array_signature_e2e` on the CLASS-MEMBER and CONSTRUCTOR path.
//!
//! The facade writer (`metadata::builder`) and the class writer (`metadata::class_builder`) encode
//! value parameters through separate code, so the two rules a signature holding an array obeys —
//! a `vararg` records `Array<out E>`, and only a `kotlin/Array` forces an explicit
//! `JvmMethodSignature.desc` — have to be proven on both. See `docs/METADATA_NOTES.md`.

use super::common;

/// Assert one same-module fixture is byte-identical to kotlinc's output for `class_internal`.
///
/// The stdlib is on the classpath because kotlinc's always is — see the sibling facade suite.
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

/// A member `vararg` of a REFERENCE element: recorded as `Array<out E>` — the projection source
/// never wrote — and, being an `Array`, it records the descriptor. It declares no default, so
/// `ValueParameter.flags` stays absent.
#[test]
fn a_member_reference_vararg_records_a_projected_array_and_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Member {\n\
        \x20   fun m(vararg xs: String): Int = xs.size\n\
        }\n";
    assert_identical("ClsVarargRef", SRC, "app/Member");
}

/// The same member with no return value, the shape that exposed a spurious
/// `DECLARES_DEFAULT_VALUE` on the vararg parameter: kotlinc writes no flags field at all.
#[test]
fn a_member_vararg_declares_no_default_value() {
    const SRC: &str = "package app\n\
        \n\
        class Member {\n\
        \x20   fun m(vararg xs: String) {}\n\
        }\n";
    assert_identical("ClsVarargUnit", SRC, "app/Member");
}

/// A plain `Array<T>` member PARAMETER — no `vararg` anywhere. The descriptor is unrecoverable for
/// exactly the same reason a vararg's is, so kotlinc records it here too; a vararg-keyed rule
/// under-records it.
#[test]
fn a_member_array_parameter_records_its_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Arr {\n\
        \x20   fun a(xs: Array<String>): Int = xs.size\n\
        }\n";
    assert_identical("ClsArrParam", SRC, "app/Arr");
}

/// An `Array<T>` member RETURN, and a nullable one — the rule scans every signature position and
/// the `?` must not hide the classifier.
#[test]
fn a_member_array_return_and_nullable_parameter_record_their_descriptors() {
    const SRC: &str = "package app\n\
        \n\
        class Arr2 {\n\
        \x20   fun make(): Array<String> = arrayOf()\n\
        \x20   fun maybe(xs: Array<String>?): Int = xs?.size ?: 0\n\
        }\n";
    assert_identical("ClsArrRet", SRC, "app/Arr2");
}

/// A member `vararg` of a PRIMITIVE element is an `IntArray`, which the reader's descriptor table
/// maps on its own — kotlinc records NO descriptor, and neither may krusty. The boundary case a
/// vararg-keyed rule over-records.
#[test]
fn a_member_primitive_vararg_records_no_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Prim {\n\
        \x20   fun m(vararg xs: Int): Int = xs.size\n\
        }\n";
    assert_identical("ClsVarargPrim", SRC, "app/Prim");
}

/// A declared `IntArray` member parameter, and a `List<Array<String>>` — an array NESTED inside
/// another type is erased away before the descriptor, so the outer `List` maps cleanly.
#[test]
fn a_member_primitive_array_and_nested_array_record_no_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Plain {\n\
        \x20   fun ints(xs: IntArray): Int = xs.size\n\
        \x20   fun nested(xs: List<Array<String>>): Int = xs.size\n\
        }\n";
    assert_identical("ClsArrNested", SRC, "app/Plain");
}

/// A CONSTRUCTOR's `vararg` records the same projected array. Constructors carry their descriptor
/// unconditionally, so this row measures the projection alone.
#[test]
fn a_constructor_reference_vararg_records_a_projected_array() {
    const SRC: &str = "package app\n\
        \n\
        class Ctor(vararg val xs: String)\n";
    assert_identical("ClsCtorVararg", SRC, "app/Ctor");
}

/// A PROPERTY's backing field obeys the same rule as a method's descriptor: an `Array` field's
/// descriptor depends on its type argument, so kotlinc records an explicit `JvmFieldSignature.desc`
/// — and records none for the `IntArray` its table maps.
#[test]
fn an_array_property_records_its_field_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class Holder {\n\
        \x20   val xs: Array<String> = arrayOf()\n\
        \x20   val ns: IntArray = intArrayOf()\n\
        }\n";
    assert_identical("ClsArrProp", SRC, "app/Holder");
}

/// A COMPANION property's field is hoisted to a static on the outer class, and travels a different
/// `PropMeta` construction than an instance property — the field-descriptor rule has to reach it too.
#[test]
fn a_companion_array_property_records_its_field_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class K {\n\
        \x20   companion object {\n\
        \x20       val xs: Array<String> = arrayOf()\n\
        \x20       val ns: IntArray = intArrayOf()\n\
        \x20   }\n\
        }\n";
    assert_identical("ClsCompanionArr", SRC, "app/K$Companion");
}

/// The same rule on a property with NO ACCESSOR to read a descriptor off: a `private val` gets no
/// getter, yet kotlinc still records its field descriptor — so the descriptor has to come from the
/// FIELD, not the getter's return type. (`@JvmField` is the other accessor-less shape; krusty has a
/// separate, unrelated gap there — it emits a getter and drops the annotation — so it is not a row
/// here.)
#[test]
fn an_accessorless_array_property_records_its_field_descriptor() {
    const SRC: &str = "package app\n\
        \n\
        class H {\n\
        \x20   private val xs: Array<String> = arrayOf()\n\
        \x20   fun n(): Int = xs.size\n\
        }\n";
    assert_identical("ClsArrPropNoGetter", SRC, "app/H");
}

/// A body property may share a PLAIN (non-`val`) vararg parameter's name without being it — its own
/// type is recorded, unprojected. The vararg-property rule keys on the parameter declaring a
/// property, not on the name alone.
#[test]
fn a_body_property_sharing_a_plain_vararg_parameter_name_keeps_its_own_type() {
    const SRC: &str = "package app\n\
        \n\
        class Shadow(vararg xs: String) {\n\
        \x20   val xs: Array<Int> = arrayOf(xs.size)\n\
        }\n";
    assert_identical("ClsShadowVararg", SRC, "app/Shadow");
}

/// A SECONDARY constructor's `vararg`, which travels a different `CtorMeta` path than the primary.
#[test]
fn a_secondary_constructor_reference_vararg_records_a_projected_array() {
    const SRC: &str = "package app\n\
        \n\
        class Ctor2(val n: Int) {\n\
        \x20   constructor(vararg xs: String) : this(xs.size)\n\
        }\n";
    assert_identical("ClsCtor2Vararg", SRC, "app/Ctor2");
}
