//! Diagnostics whose wording or position differs between supported kotlinc releases. Each case's
//! complete error ledger is recorded per Kotlin version in
//! `tests/recorded/diagnostic_wording_versions_e2e.txt`
//! from that version's kotlinc (see `tests/common/recorded.rs`), and krusty must report exactly
//! the ledger recorded for the reference version under test (`KRUSTY_LANGUAGE_VERSION`), so
//! `just test-all` checks every release's spelling.
//!
//! The spellings krusty emits live in `krusty::diagnostic_wording`; these tests read the recorded
//! kotlinc output rather than that table, so a wrong table row fails here.

use super::common;

/// `+MultiPlatformProjects` for krusty, as a source directive; kotlinc gets `-Xmulti-platform`. It
/// takes line 1, so a multiplatform fixture's declarations start on line 2.
const MULTIPLATFORM: &str = "// LANGUAGE: +MultiPlatformProjects\n";

/// Assert krusty reports exactly the ledger kotlinc reports for `source` under the reference
/// version, as recorded for the running test.
fn assert_matches_kotlinc(file: &str, source: &str) {
    let reference_args = if source.starts_with(MULTIPLATFORM) {
        vec!["-Xmulti-platform".to_string()]
    } else {
        Vec::new()
    };
    common::assert_errors_match_kotlinc(&[(file, source)], &reference_args);
}

/// UNRESOLVED_REFERENCE on an explicit receiver: 2.4.20 names the receiver's type.
#[test]
fn unresolved_member() {
    assert_matches_kotlinc("Member.kt", "fun f(x: String): Int = x.missing\n");
}

/// A classifier qualifier and a type-parameter value are not class-like receiver values, so no
/// release names a receiver type for them.
#[test]
fn unresolved_member_of_a_qualifier_or_type_parameter() {
    assert_matches_kotlinc(
        "Qualifier.kt",
        "class Limits { companion object { const val MAX: Int = 1 } }\n\
         object Obj\n\
         fun a(): Int = Limits.MISSING\n\
         fun b(): Int = Obj.MISSING\n\
         fun <T> c(t: T) { t.missing }\n",
    );
}

/// NO_VALUE_FOR_PARAMETER: same words, but 2.4.20 reports it at the callee's name instead of the
/// argument list.
#[test]
fn missing_argument() {
    assert_matches_kotlinc("Missing.kt", "fun h(x: Int): Int = x\nfun g(): Int = h()\n");
}

/// NO_ACTUAL_FOR_EXPECT: 2.4.20 quotes the modifiers, the name and the module.
#[test]
fn unactualized_expect() {
    assert_matches_kotlinc(
        "NoActual.kt",
        &format!("{MULTIPLATFORM}expect fun helper(): Int\n"),
    );
}

/// ACTUAL_WITHOUT_EXPECT: 2.4.20 says `'expect' declaration` for `expected declaration`.
#[test]
fn stray_actual() {
    assert_matches_kotlinc(
        "Stray.kt",
        &format!("{MULTIPLATFORM}actual fun stray(): Int = 1\n"),
    );
}

/// EXPECTED_DECLARATION_WITH_BODY, EXPECTED_PROPERTY_INITIALIZER, EXPECTED_DELEGATED_PROPERTY:
/// 2.4.20 says `'expect'` for `expected`. Any of them suppresses the unmatched-expect report.
#[test]
fn implemented_expect() {
    assert_matches_kotlinc(
        "Body.kt",
        &format!(
            "{MULTIPLATFORM}expect fun f(): Int = 1\nexpect val p: Int = 2\n\
             expect val d: Int by lazy {{ 1 }}\n"
        ),
    );
}
