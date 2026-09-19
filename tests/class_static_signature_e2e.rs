//! A CLASS-owned static field's generic `Signature`.
//!
//! Three sites decide whether a field records one — the file facade, an instance field, and a
//! static owned by a class — and only the first two asked. The class-owned path wrote through the
//! one field writer that takes no signature argument, so a parameterized static declared nothing
//! but its erasure.
//!
//! The rule is not serialization's: any `@JvmField` companion property lands on the outer class as
//! a static, and every one whose Kotlin type carries type arguments was wrong. That is what this
//! file pins, on a fixture that names nothing from the stdlib's serialization surface. The
//! serialization plugin's `$childSerializers` is covered separately, as integration.
//!
//! Every step fails closed: a missing reference compiler or a failed krusty compile is a test
//! failure, not a skip. A signature regression that reports success is worse than no test.
use super::common;
use super::serialization_companion_byte_parity_e2e::{compare_with_kotlinc_plugin, structure};

/// A `@JvmField` companion property is emitted on the OUTER class as a `public static final` field
/// — the plainest class-owned static Kotlin has, and one this repository owns outright.
const SRC: &str = "class Coffer {\n\
                   \x20   companion object {\n\
                   \x20       @JvmField\n\
                   \x20       val manifest: List<String> = listOf()\n\
                   \x20\n\
                   \x20       @JvmField\n\
                   \x20       val ledger: Map<String, List<Int>> = mapOf()\n\
                   \x20\n\
                   \x20       @JvmField\n\
                   \x20       val tally: String = \"\"\n\
                   \x20   }\n\
                   }\n";

/// The field table as javap prints it: every declaration up to the first method.
fn fields(disassembly: &str) -> Vec<String> {
    structure(disassembly)
        .into_iter()
        .skip_while(|line| !line.contains(" manifest;"))
        .take_while(|line| !line.contains("Coffer();"))
        .collect()
}

/// The `Signature:` javap prints for one field, with its constant-pool index erased.
fn field_signature(disassembly: &str, field: &str) -> Option<String> {
    disassembly
        .lines()
        .skip_while(|line| !line.contains(&format!(" {field};")))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| {
            line.trim()
                .strip_prefix("Signature:")?
                .split("// ")
                .nth(1)
                .map(str::to_string)
        })
}

fn built() -> super::serialization_companion_byte_parity_e2e::ReferenceComparison {
    compare_with_kotlinc_plugin(
        "CofferStatics",
        SRC,
        "Coffer",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("the reference compiler and javap must be available to this regression")
}

/// The whole field table matches kotlinc's, signatures included.
///
/// Comparing the table rather than the class's bytes is deliberate and narrow: `Coffer` still
/// differs from kotlinc by two constant-pool entries and two `<clinit>` line-table rows, neither
/// of which this rule reaches. The field table is exactly what the rule decides, so that is what
/// is asserted — and it is asserted against the reference, not spelled out, so it cannot drift.
#[test]
fn a_class_owned_static_field_table_matches_kotlinc() {
    let built = built();
    assert_eq!(
        fields(&built.krusty),
        fields(&built.reference),
        "the class-owned static field table must match kotlinc's"
    );
}

/// The literal signatures, spelled out — so a change in what the reference writes is visible here
/// rather than silently agreed with, and so the nesting of a type argument inside a type argument
/// is pinned rather than assumed from one flat case.
#[test]
fn a_parameterized_class_owned_static_records_its_signature_literally() {
    let built = built();
    assert_eq!(
        field_signature(&built.reference, "manifest").as_deref(),
        Some("Ljava/util/List<Ljava/lang/String;>;"),
        "the reference's own record for a one-argument type:\n{}",
        built.reference
    );
    assert_eq!(
        field_signature(&built.krusty, "manifest").as_deref(),
        Some("Ljava/util/List<Ljava/lang/String;>;"),
        "krusty must record it too:\n{}",
        built.krusty
    );
    assert_eq!(
        field_signature(&built.reference, "ledger").as_deref(),
        Some("Ljava/util/Map<Ljava/lang/String;Ljava/util/List<Ljava/lang/Integer;>;>;"),
        "the reference's own record for a NESTED type argument:\n{}",
        built.reference
    );
    assert_eq!(
        field_signature(&built.krusty, "ledger").as_deref(),
        Some("Ljava/util/Map<Ljava/lang/String;Ljava/util/List<Ljava/lang/Integer;>;>;"),
        "krusty must record the nested one too:\n{}",
        built.krusty
    );
}

/// A static whose Kotlin type has NO type arguments records nothing — the rule is "erasure loses
/// something", not "the field is a static". Without this the fix could be a blanket attribute and
/// the table above would still match on the two parameterized rows.
#[test]
fn a_class_owned_static_with_no_type_arguments_records_no_signature() {
    let built = built();
    assert_eq!(
        field_signature(&built.reference, "tally"),
        None,
        "the reference records none for a `String`:\n{}",
        built.reference
    );
    assert_eq!(
        field_signature(&built.krusty, "tally"),
        None,
        "krusty must record none either:\n{}",
        built.krusty
    );
}
