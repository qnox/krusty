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
