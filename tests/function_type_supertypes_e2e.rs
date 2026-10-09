//! `FunctionalTypeWithExtensionAsSupertype`: an extension or context function type as the supertype
//! of a class, an interface or an object expression. Without the feature kotlinc reports
//! `SUPERTYPE_IS_EXTENSION_OR_CONTEXT_FUNCTION_TYPE` at each such supertype reference; a plain
//! function type stays a valid supertype. The complete error ledger must be the expected one for
//! both compilers, and with the feature both run the fixture to "OK".

use super::common;

const NOT_ALLOWED: &str = "extension or contextual function type is not allowed as a supertype.";

const SUPERTYPES: &str = r#"class A : String.(Int) -> String {
    override fun invoke(p1: String, p2: Int) = p1 + p2
}
class P : (Int) -> Int {
    override fun invoke(p1: Int) = p1
}
interface I : context(String) () -> String
fun box(): String {
    val o = object : Int.() -> Int {
        override fun invoke(p1: Int) = p1 + 1
    }
    val all = A()("k", 1) + P()(2) + o(3)
    return if (all == "k124") "OK" else "fail: $all"
}
"#;

#[test]
fn extension_and_context_function_supertypes_require_the_feature() {
    let sources = [("Main.kt", SUPERTYPES)];
    let expected = ["1:11", "7:15", "9:22"].map(|at| format!("Main.kt:{at}: {NOT_ALLOWED}"));
    assert_eq!(
        common::reference_error_ledger(&sources, &[]),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_ledger_with_args(&sources, &[]),
        expected
    );
}

#[test]
fn extension_and_context_function_supertypes_run_with_the_feature() {
    let source = format!("// LANGUAGE: +FunctionalTypeWithExtensionAsSupertype\n{SUPERTYPES}");
    common::expect_box_same_as_kotlinc(&source, "FunctionSupertypes");
}
