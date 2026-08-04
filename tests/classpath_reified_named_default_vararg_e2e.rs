//! mockk's exact signature shape: a CLASSPATH top-level reified inline fn with defaulted
//! parameters on both sides of a NON-FINAL vararg and a trailing receiver-lambda default:
//!   `inline fun <reified T : Any> mockk(name: String? = null, relaxed: Boolean = false,
//!    vararg moreInterfaces: KClass<*>, relaxUnitFun: Boolean = false, block: T.() -> Unit = {}): T`
//! `mockk<C>()` resolved, but adding one NAMED argument — `mockk<C>(relaxed = true)` — failed
//! to resolve with the explicit type argument bound, so the property type collapsed to
//! "cannot infer the type of property 'client'".
use super::common;

const LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    inline fun <reified T : Any> mockk(\n\
    \x20 name: String? = null,\n\
    \x20 relaxed: Boolean = false,\n\
    \x20 vararg moreInterfaces: KClass<*>,\n\
    \x20 relaxUnitFun: Boolean = false,\n\
    \x20 block: T.() -> Unit = {},\n\
    ): T = T::class.java.getDeclaredConstructor().newInstance().apply(block)\n";

#[test]
fn classpath_reified_named_default_before_vararg_resolves() {
    // The named argument sits BEFORE the omitted vararg; every other parameter is defaulted.
    // BOTH forms: the full checker's local `val`, and the mockk test-class shape — a CLASS
    // PROPERTY, whose type is inferred by the SIGNATURE phase's lightweight inferer.
    const MAIN: &str = "import lib.mockk\n\
        class C {\n\
        \x20 fun hi(): String = \"OK\"\n\
        }\n\
        class Harness {\n\
        \x20 private val client = mockk<C>(relaxed = true)\n\
        \x20 fun use(): String = client.hi()\n\
        }\n\
        fun use(): String {\n\
        \x20 val client = mockk<C>(relaxed = true)\n\
        \x20 return client.hi()\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against("cp_reified_named_default", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "named default before omitted vararg must resolve with the explicit type argument"
    );
}

#[test]
fn classpath_reified_named_default_after_vararg_resolves() {
    // The named argument sits AFTER the omitted vararg (`relaxUnitFun`), and a combined form
    // names arguments on BOTH sides of it.
    const MAIN: &str = "import lib.mockk\n\
        class C {\n\
        \x20 fun hi(): String = \"OK\"\n\
        }\n\
        class Harness {\n\
        \x20 private val a = mockk<C>(relaxUnitFun = true)\n\
        \x20 private val b = mockk<C>(relaxed = true, relaxUnitFun = true)\n\
        \x20 fun use(): String = a.hi() + b.hi()\n\
        }\n\
        fun use(): String {\n\
        \x20 val a = mockk<C>(relaxUnitFun = true)\n\
        \x20 val b = mockk<C>(relaxed = true, relaxUnitFun = true)\n\
        \x20 return a.hi() + b.hi()\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against("cp_reified_named_default_after", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "named defaults around an omitted vararg must resolve with the explicit type argument"
    );
}

#[test]
fn classpath_reified_trailing_receiver_lambda_resolves() {
    // The trailing receiver-lambda default (`block: T.() -> Unit = {}`): its implicit `this`
    // must bind to the EXPLICIT type argument, not to `T`'s bound (`kotlin/Any` made every
    // member access inside the block "unresolved reference"). With and without a named
    // argument before it, in both the property-initializer and local-`val` positions.
    const MAIN: &str = "import lib.mockk\n\
        class C {\n\
        \x20 var tag: String = \"\"\n\
        }\n\
        class Harness {\n\
        \x20 private val plain = mockk<C> { tag = \"!\" }\n\
        \x20 private val named = mockk<C>(relaxed = true) { tag = \"!\" }\n\
        \x20 fun use(): String = plain.tag + named.tag\n\
        }\n\
        fun use(): String {\n\
        \x20 val plain = mockk<C> { tag = \"!\" }\n\
        \x20 val named = mockk<C>(relaxed = true) { tag = \"!\" }\n\
        \x20 return plain.tag + named.tag\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against("cp_reified_trailing_lambda", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "trailing receiver-lambda must bind its receiver from the explicit type argument"
    );
}

// The same parameter geometry WITHOUT `inline`/`reified` (whose `T::class` body is the splice
// refusal the backend declines by design): a leading seed supplies the instance, everything
// after it mirrors mockk — defaults on both sides of a non-final vararg, trailing
// receiver-lambda default.
const RUNTIME_LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    fun <T : Any> mkd(\n\
    \x20 seed: T,\n\
    \x20 name: String? = null,\n\
    \x20 relaxed: Boolean = false,\n\
    \x20 vararg moreInterfaces: KClass<*>,\n\
    \x20 relaxUnitFun: Boolean = false,\n\
    \x20 block: T.() -> Unit = {},\n\
    ): T {\n\
    \x20 require(moreInterfaces.isEmpty())\n\
    \x20 if (relaxed && name == null && !relaxUnitFun) block(seed)\n\
    \x20 return seed\n\
    }\n";

#[test]
fn classpath_named_default_before_vararg_box_runs() {
    // End to end: the `$default` bridge call must pack the omitted vararg, put the named
    // argument in its declared slot, default the rest, and hand the trailing lambda its
    // receiver — checked by the RUNTIME effect of each form.
    const MAIN: &str = "import lib.mkd\n\
        class C {\n\
        \x20 var tag: String = \"\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val plain = mkd(C())\n\
        \x20 if (plain.tag != \"\") return \"F1:\" + plain.tag\n\
        \x20 val named = mkd(C(), relaxed = true)\n\
        \x20 if (named.tag != \"\") return \"F2:\" + named.tag\n\
        \x20 val tagged = mkd(C(), relaxed = true) { tag = \"!\" }\n\
        \x20 if (tagged.tag != \"!\") return \"F3:\" + tagged.tag\n\
        \x20 val after = mkd(C(), relaxUnitFun = true) { tag = \"?\" }\n\
        \x20 if (after.tag != \"\") return \"F4:\" + after.tag\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) =
        common::expect_box_run_against("cp_named_default_vararg_box", RUNTIME_LIB, MAIN)
    {
        assert_eq!(out, "OK");
    }
}
