//! A callable reference none of whose candidates fits the expected function shape. kotlinc reports
//! it by where the reference stands: as a call's value argument its candidates are inapplicable,
//! elsewhere a single nearest candidate gives the reference its reflection type for the enclosing
//! check, and several nearest candidates are listed.

use super::common;

fn assert_kotlinc_errors(src: &str, expected: &[&str]) {
    let sources = [("Main.kt", src)];
    let reference_args = common::language_directives::kotlinc_args(src);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        expected
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}

#[test]
fn a_hidden_member_leaves_the_visible_extension_to_mismatch_the_initializer() {
    assert_kotlinc_errors(
        "open class B { protected fun f(i: Int): String = \"f\" }\n\
fun B.f(i: Int, j: Int): String = \"e\"\n\
val ref: (B, Int) -> String = B::f\n",
        &["Main.kt:3:29: initializer type mismatch: expected '(B, Int) -> String', actual 'KFunction3<B, Int, Int, String>'."],
    );
}

#[test]
fn several_misfit_extensions_in_an_initializer_are_listed() {
    assert_kotlinc_errors(
        "class B\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun B.g(s: String, t: String, u: String): String = \"e\"\n\
val ref: (B, Int) -> String = B::g\n",
        &[
            "Main.kt:4:34: none of the following candidates is applicable:",
            "| ",
            "| fun B.g(i: Int, j: Int): String",
            "| fun B.g(s: String, t: String, u: String): String",
        ],
    );
}

#[test]
fn a_misfit_argument_reference_reports_its_inapplicable_candidate() {
    assert_kotlinc_errors(
        "class B\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun take(f: (B, Int) -> String) = f\n\
val r = take(B::g)\n",
        &["Main.kt:4:17: inapplicable candidate(s): fun B.g(i: Int, j: Int): String"],
    );
}

#[test]
fn several_misfit_argument_candidates_give_each_reason() {
    assert_kotlinc_errors(
        "class B { fun g(s: String): String = s }\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun take(f: (B, Int) -> String) = f\n\
fun use() { take(B::g) }\n",
        &[
            "Main.kt:4:21: none of the following candidates is applicable:",
            "| ",
            "| fun g(s: String): String:",
            "|   Inapplicable candidate(s): fun g(s: String): String",
            "| ",
            "| fun B.g(i: Int, j: Int): String:",
            "|   Inapplicable candidate(s): fun B.g(i: Int, j: Int): String",
        ],
    );
}

#[test]
fn the_member_level_decides_a_misfit_initializer() {
    assert_kotlinc_errors(
        "class B { fun g(s: String): String = s }\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun use() { val ref: (B, Int) -> String = B::g }\n",
        &["Main.kt:3:41: initializer type mismatch: expected '(B, Int) -> String', actual 'KFunction2<B, String, String>'."],
    );
}

#[test]
fn a_misfit_reference_mismatches_a_return_and_an_assignment() {
    assert_kotlinc_errors(
        "class B\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun h(): (B, Int) -> String = B::g\n\
fun f() { var ref: (B, Int) -> String = { _, _ -> \"\" }; ref = B::g }\n",
        &[
            "Main.kt:3:31: return type mismatch: expected '(B, Int) -> String', actual 'KFunction3<B, Int, Int, String>'.",
            "Main.kt:4:61: assignment type mismatch: actual type is 'KFunction3<B, Int, Int, String>', but '(B, Int) -> String' was expected.",
        ],
    );
}

#[test]
fn safe_call_and_enum_entry_arguments_report_their_inapplicable_candidate() {
    assert_kotlinc_errors(
        "class B\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
class T { fun take(f: (B, Int) -> String) = f }\n\
fun use(t: T?) { t?.take(B::g) }\n\
enum class E(f: (B, Int) -> String) { X(B::g) }\n",
        &[
            "Main.kt:4:29: inapplicable candidate(s): fun B.g(i: Int, j: Int): String",
            "Main.kt:5:44: inapplicable candidate(s): fun B.g(i: Int, j: Int): String",
        ],
    );
}

#[test]
fn superclass_constructor_arguments_report_their_candidates() {
    assert_kotlinc_errors(
        "class B\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun B.k(i: Int, j: Int): String = \"e\"\n\
fun B.k(s: String, t: String, u: String): String = s\n\
open class P(f: (B, Int) -> String)\n\
class C : P(B::g)\n\
class D : P(B::k)\n",
        &[
            "Main.kt:6:16: inapplicable candidate(s): fun B.g(i: Int, j: Int): String",
            "Main.kt:7:16: none of the following candidates is applicable:",
            "| ",
            "| fun B.k(i: Int, j: Int): String:",
            "|   Inapplicable candidate(s): fun B.k(i: Int, j: Int): String",
            "| ",
            "| fun B.k(s: String, t: String, u: String): String:",
            "|   Inapplicable candidate(s): fun B.k(s: String, t: String, u: String): String",
        ],
    );
}

#[test]
fn a_misfit_default_value_mismatches_its_parameter_like_an_initializer() {
    assert_kotlinc_errors(
        "class B\n\
fun B.g(i: Int, j: Int): String = \"e\"\n\
fun h(f: (B, Int) -> String = B::g) = f\n",
        &["Main.kt:3:29: initializer type mismatch: expected '(B, Int) -> String', actual 'KFunction3<B, Int, Int, String>'."],
    );
}

/// The fitting forms of a superclass constructor's reference argument: the selected parameter picks
/// the overload, and a parameter that is not a function type takes the natural reference.
const SUPERCLASS_REFERENCE_ARGUMENTS: &str = r#"class B

fun B.g(i: Int): String = "g$i"
fun B.g(s: String, t: String): String = s + t
fun B.h(): Int = 7

open class P(f: (B, Int) -> String) {
    val selected = f is Function2<*, *, *>
}
class C : P(B::g)

open class Q(a: Any) {
    val named = a is Function1<*, *>
}
class D : Q(B::h)

fun box(): String {
    if (!C().selected) return "fail selected overload"
    if (!D().named) return "fail natural reference"
    return "OK"
}
"#;

#[test]
fn superclass_constructor_reference_arguments_run_and_match_kotlinc() {
    let src = SUPERCLASS_REFERENCE_ARGUMENTS;
    common::expect_box_ok_with_stdlib(src, "SuperclassReferenceArguments");
    for class in ["C", "D"] {
        match common::byte_diff_against_kotlinc_cp(
            "SuperclassReferenceArguments",
            src,
            class,
            &[common::stdlib_jar()],
        ) {
            None => panic!("reference toolchain unavailable"),
            Some(result) => result.unwrap_or_else(|diff| panic!("{class}: {diff}")),
        }
    }
}
