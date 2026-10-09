//! `LocalTypeAliases` (`-Xlocal-type-aliases`): a type alias declared in a body. Without the feature
//! kotlinc reports `UNSUPPORTED_FEATURE` at the start of each such declaration, its annotations
//! included: in a function or local function, a member function, an initializer block and an
//! object expression. Both compilers compile the fixture; the complete error ledger must be the
//! expected one for both, and with the argument both run it to "OK".

use super::common;

const UNSUPPORTED: &str = "the feature \"local type aliases\" is experimental and should be \
    enabled explicitly. This can be done by supplying the compiler argument \
    '-Xlocal-type-aliases', but note that no stability guarantees are provided.";

const LOCAL_ALIASES: &str = r#"class Outer {
    fun m(): Int {
        typealias InMember = String
        val o = object {
            typealias InObject = Int
            fun v(): Int = 1
        }
        val s: InMember = "a"
        return s.length + o.v()
    }
    init {
        typealias InInit = Int
        val i: InInit = 0
    }
}
fun box(): String {
    @Suppress("TOPLEVEL_TYPEALIASES_ONLY") typealias Annotated = Int
    fun local(): Int {
        typealias InLocalFun = Int
        val one: InLocalFun = 1
        return one
    }
    typealias Generic<T> = List<T>
    val x: Generic<Annotated> = listOf(1)
    return if (Outer().m() + x[0] + local() == 4) "OK" else "fail"
}
"#;

#[test]
fn local_type_aliases_require_the_feature() {
    let sources = [("Main.kt", LOCAL_ALIASES)];
    let expected = ["3:9", "5:13", "12:9", "17:5", "19:9", "23:5"]
        .map(|at| format!("Main.kt:{at}: {UNSUPPORTED}"));
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
fn local_type_aliases_run_with_the_argument() {
    common::expect_box_same_as_kotlinc_with_args(
        LOCAL_ALIASES,
        "LocalTypeAliases",
        &["-Xlocal-type-aliases"],
    );
}
