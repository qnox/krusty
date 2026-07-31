//! Cross-module contract round-trip: a library module compiled by krusty emits its functions'
//! `contract { … }` effects into `@Metadata` (`Function.contract`, field 32); the app module,
//! compiled against the library's classes, decodes them and smart-casts at the call site —
//! including a type-parameter conclusion (`value is R`) substituted with the call's type
//! arguments. Mirrors `contracts/referenceToGenericInDeserializedContract.kt` (KT-76301).

use super::common;

const LIB: &str = "import kotlin.contracts.contract\n\
import kotlin.contracts.ExperimentalContracts\n\
@OptIn(ExperimentalContracts::class)\n\
inline fun <T, reified R> Refinement<T, R>.validate(value: T): Boolean {\n\
    contract { returns() implies (value is R) }\n\
    return isValid(value)\n\
}\n\
class Refinement<T, R> {\n\
    fun isValid(value: T): Boolean = value is String\n\
}\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, jdk.clone()], Some(&jdk))
}

#[test]
fn cross_module_inline_reified_call_lowers() {
    // The baseline: the cross-module inline reified call itself must lower (no contract use).
    const MAIN: &str = "fun box(): String {\n\
        val r = Refinement<Any, String>()\n\
        return if (r.validate(\"x\")) \"OK\" else \"FAIL\"\n\
    }\n";
    assert_eq!(
        run("rgdc_call", MAIN).expect("cross-module inline reified call"),
        "OK"
    );
}

#[test]
fn deserialized_contract_smart_casts_with_call_site_type_args() {
    // `returns() implies (value is R)` from the library's @Metadata; R binds to String here.
    // The `.length` member access type-checks ONLY through the smart cast — `Any` has no `length`.
    const MAIN: &str = "fun test_fromLib(r: Refinement<Any, String>, x: Any): Int {\n\
        r.validate(x)\n\
        return x.length\n\
    }\n\
    fun box(): String {\n\
        val r = Refinement<Any, String>()\n\
        return if (test_fromLib(r, \"OK\") == 2) \"OK\" else \"FAIL\"\n\
    }\n";
    assert_eq!(
        run("rgdc_cast", MAIN).expect("deserialized contract smart cast"),
        "OK"
    );
}

const CONTEXT_LIB: &str = "import kotlin.contracts.ExperimentalContracts\n\
import kotlin.contracts.contract\n\
@OptIn(ExperimentalContracts::class)\n\
context(a: String?)\n\
fun validate1() {\n\
    contract { returns() implies (a != null) }\n\
    a!!\n\
}\n";

#[test]
fn context_parameter_contract_round_trip_cross_module() {
    // Mirrors contracts/contractOnContextParameter.kt: a context-parameter function's contract
    // (`returns() implies (a != null)`) rides `@Metadata` (`Function.context_parameter` = 13 +
    // `contract` = 32); the caller's implicit context source (`with(…)`'s `this`) smart-casts.
    const MAIN: &str = "fun box(): String {\n\
        return with(\"O\" as String?) {\n\
            validate1()\n\
            this\n\
        } + \"K\"\n\
    }\n";
    let jdk = common::jdk_modules().expect("jdk");
    let sl = common::stdlib_jar().expect("stdlib");
    let lo = common::compile_lib("ctx_p", CONTEXT_LIB).expect("lib compiles");
    let out = common::compile_and_run_box(MAIN, "Main", &[lo, sl, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.expect("context contract smart cast compiles + runs"),
        "OK"
    );
}
