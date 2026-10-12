//! `external` written on an enum entry or on a primary-constructor property
//! (`ForbidExternalEnumEntriesAndPrimaryConstructorProperties`, KTLC-389). kotlinc's modifier
//! checker accepts both with a deprecation warning while the feature is off, and rejects them as
//! inapplicable once it is on (language 2.5, or `-progressive` on 2.4). The check is common to
//! every target. kotlinc prints no warnings for a compilation with an error, so each fixture keeps
//! to one outcome.

use super::common;

const FEATURE: &str = "ForbidExternalEnumEntriesAndPrimaryConstructorProperties";

const SOURCE: &str = r#"enum class E(external val v: Int) {
    @Suppress("UNUSED") external A(1),
    external
    B(2) { },
    C(3)
}
class K(external val x: Int, external var y: String, z: Int)
data class D(val a: Int, external val d: Int)
annotation class N(external val n: Int)
fun use() = K(1, "", 2).x + E.A.v + D(1, 2).d
"#;

/// Each outcome compared with kotlinc: exit code, then every error and every warning with its
/// file, line, column and message, in emission order.
fn assert_reported_like_kotlinc(source: &str, reference_args: &[String]) {
    let result = common::compiler_diagnostics_with_reference_args(
        &[("Use.kt", source)],
        &[common::stdlib_jar()],
        reference_args,
    );
    let reported = |code: i32, stderr: &str| {
        (
            code,
            common::compiler_errors(stderr),
            common::compiler_warnings(stderr),
        )
    };
    assert_eq!(
        reported(result.krusty_code, &result.krusty_stderr),
        reported(result.reference_code, &result.reference_stderr),
        "\nkrusty: {}\nkotlinc: {}",
        result.krusty_stderr,
        result.reference_stderr
    );
}

/// Without the feature, each `external` is deprecated for its target and the code compiles.
#[test]
fn external_on_an_enum_entry_or_a_constructor_property_is_deprecated() {
    assert_reported_like_kotlinc(SOURCE, &[]);
}

/// With the feature on, each `external` is not applicable to its target.
#[test]
fn external_on_an_enum_entry_or_a_constructor_property_is_rejected_by_the_feature() {
    assert_reported_like_kotlinc(
        &format!("// LANGUAGE: +{FEATURE}\n{SOURCE}"),
        &[format!("-XXLanguage:+{FEATURE}")],
    );
}

/// An entry named `external`, and an `external`-named classifier after the entry list, are not
/// modifiers.
#[test]
fn an_entry_named_external_is_not_a_modifier() {
    assert_reported_like_kotlinc(
        "enum class G { external, internal, B;\n    fun f() = 1\n}\n\
         enum class H { A,\n    ;\n    class external\n}\n\
         fun g() = G.external.ordinal + H.A.ordinal\n",
        &[],
    );
}
