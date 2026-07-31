//! `callsInPlace(x, EXACTLY_ONCE)` contract effects compile and run correctly: the effect is
//! parsed from source, decoded from / emitted to `@Metadata` (the round trip is covered by
//! `contract_metadata_roundtrip_e2e`), and these programs lower to working code — a captured
//! `var`/field write from an exactly-once lambda is Ref-boxed / written through the captured
//! `this`. Call-site SEMANTICS (definite assignment, captured-var permission) are NOT yet derived
//! from `callsInPlace` — the checker has no `Effect::CallsInPlace` consumer; these tests pin the
//! shapes so a future application can't regress the compile+run behavior.
//! Mirrors `contracts/constructorArgument.kt` (non-inline, init lambda) and
//! `contracts/fieldInConstructorParens.kt` (private inline member, val-field write).

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn non_inline_exactly_once_lambda_writes_captured_var_in_init() {
    const SRC: &str = "import kotlin.contracts.*\n\
@OptIn(kotlin.contracts.ExperimentalContracts::class)\n\
fun runOnce(action: () -> Unit) {\n\
    contract { callsInPlace(action, InvocationKind.EXACTLY_ONCE) }\n\
    action()\n\
}\n\
class Foo(foo: Boolean) {\n\
    var res = \"FAIL\"\n\
    init {\n\
        runOnce {\n\
            foo\n\
            res = \"OK\"\n\
        }\n\
    }\n\
}\n\
fun box(): String = Foo(true).res\n";
    assert_eq!(
        run(SRC).expect("EO lambda captured-var write compiles + runs"),
        "OK"
    );
}

#[test]
fn inline_member_exactly_once_lambda_initializes_val_field() {
    const SRC: &str = "import kotlin.contracts.*\n\
class Smth {\n\
    val whatever: Int\n\
    init {\n\
        calculate({ whatever = it })\n\
    }\n\
    @OptIn(ExperimentalContracts::class)\n\
    private inline fun calculate(block: (Int) -> Unit) {\n\
        contract { callsInPlace(block, InvocationKind.EXACTLY_ONCE) }\n\
        block(42)\n\
    }\n\
}\n\
fun box(): String = if (Smth().whatever == 42) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("inline EO lambda val-field write compiles + runs"),
        "OK"
    );
}
