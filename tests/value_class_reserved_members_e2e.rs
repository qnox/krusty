//! kotlinc's reserved member names inside a value class (`FirValueClassDeclarationChecker`): an
//! inline value class may not declare a non-abstract `box` or `unbox`, nor, unless
//! `CustomEqualsInValueClasses` is enabled, `equals` or `hashCode`. Each is
//! `RESERVED_MEMBER_INSIDE_VALUE_CLASS` at the function's name; a full value class reserves none.

use super::common;

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors, which must also be `errors`, and the same verdict.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[&str]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("ValueClasses.kt", source)],
        &[common::stdlib_jar()],
        &args,
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let rendered = kotlinc_errors
        .iter()
        .map(|error| format!("{}:{}: {}", error.line, error.column, error.message))
        .collect::<Vec<_>>();
    assert_eq!(rendered, errors);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const MEMBERS: &str = "\
@JvmInline
value class A(val x: Int) {
    fun box(): Int = 1
    fun box(y: Int) {}
    fun unbox() {}
    override fun equals(other: Any?): Boolean = true
    override fun hashCode(): Int = 0
    fun equals(a: Any, b: Any): Boolean = true
    fun boxed() {}
}
interface WithBox {
    fun box(): String
}
@JvmInline
value class B(val s: String) : WithBox {
    override fun box(): String = s
}
class Plain {
    fun box() {}
    fun unbox() {}
}
";

#[test]
fn reserved_members_inside_a_value_class_match_kotlinc() {
    assert_matches_kotlinc(
        MEMBERS,
        &[],
        &[
            "3:9: member name 'box' is reserved for future releases.",
            "4:9: member name 'box' is reserved for future releases.",
            "5:9: member name 'unbox' is reserved for future releases.",
            "6:18: member name 'equals' is reserved for future releases.",
            "7:18: member name 'hashCode' is reserved for future releases.",
            "8:9: member name 'equals' is reserved for future releases.",
            "16:18: member name 'box' is reserved for future releases.",
        ],
    );
}

#[test]
fn custom_equals_releases_equals_and_hash_code_but_not_box() {
    assert_matches_kotlinc(
        MEMBERS,
        &["-XXLanguage:+CustomEqualsInValueClasses"],
        &[
            "3:9: member name 'box' is reserved for future releases.",
            "4:9: member name 'box' is reserved for future releases.",
            "5:9: member name 'unbox' is reserved for future releases.",
            "16:18: member name 'box' is reserved for future releases.",
        ],
    );
}

const FULL: &str = "\
value class F(val x: Int, val y: Int) {
    fun box() {}
    fun unbox() {}
}
";

#[test]
fn a_full_value_class_reserves_no_member_name() {
    assert_matches_kotlinc(FULL, &["-XXLanguage:+FullValueClasses"], &[]);
}

const SUPPRESSED: &str = "\
@file:Suppress(\"RESERVED_MEMBER_INSIDE_VALUE_CLASS\")
@JvmInline
value class A(val x: Int) {
    override fun equals(other: Any?): Boolean = other is A && x == other.x
    fun box() {}
}
";

const LOCALLY_SUPPRESSED: &str = "\
@JvmInline
@Suppress(\"RESERVED_MEMBER_INSIDE_VALUE_CLASS\")
value class A(val x: Int) {
    fun box() {}
}
@JvmInline
value class B(val x: Int) {
    @Suppress(\"RESERVED_MEMBER_INSIDE_VALUE_CLASS\")
    fun box() {}
    fun unbox() {}
}
";

#[test]
fn a_suppressed_reserved_member_is_accepted_like_kotlinc() {
    assert_matches_kotlinc(SUPPRESSED, &[], &[]);
    assert_matches_kotlinc(
        LOCALLY_SUPPRESSED,
        &[],
        &["10:9: member name 'unbox' is reserved for future releases."],
    );
}
