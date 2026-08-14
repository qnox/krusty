//! A domain-neutral top-level generic factory with defaulted parameters on both sides of a
//! NON-FINAL vararg and a trailing receiver-lambda default:
//!   `inline fun <reified T : Any> configure(label: String? = null, enabled: Boolean = false,
//!    vararg extraTypes: KClass<*>, strict: Boolean = false, block: T.() -> Unit = {}): T`
//! The fixture isolates explicit binding plus argument geometry; no external API identity participates.
use super::common;

const LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    inline fun <reified T : Any> configure(\n\
    \x20 label: String? = null,\n\
    \x20 enabled: Boolean = false,\n\
    \x20 vararg extraTypes: KClass<*>,\n\
    \x20 strict: Boolean = false,\n\
    \x20 block: T.() -> Unit = {},\n\
    ): T = T::class.java.getDeclaredConstructor().newInstance().apply(block)\n";

#[test]
fn classpath_reified_named_default_before_vararg_resolves() {
    // The named argument sits BEFORE the omitted vararg; every other parameter is defaulted.
    // Both forms cover the full checker's local `val` and a CLASS PROPERTY whose type is inferred
    // by the signature phase's lightweight inferer.
    const MAIN: &str = "import lib.configure\n\
        class C {\n\
        \x20 fun hi(): String = \"OK\"\n\
        }\n\
        class Harness {\n\
        \x20 private val client = configure<C>(enabled = true)\n\
        \x20 fun use(): String = client.hi()\n\
        }\n\
        fun use(): String {\n\
        \x20 val client = configure<C>(enabled = true)\n\
        \x20 return client.hi()\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against_ref("cp_reified_named_default", LIB, MAIN)
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
    // The named argument sits AFTER the omitted vararg (`strict`), and a combined form
    // names arguments on BOTH sides of it.
    const MAIN: &str = "import lib.configure\n\
        class C {\n\
        \x20 fun hi(): String = \"OK\"\n\
        }\n\
        class Harness {\n\
        \x20 private val a = configure<C>(strict = true)\n\
        \x20 private val b = configure<C>(enabled = true, strict = true)\n\
        \x20 fun use(): String = a.hi() + b.hi()\n\
        }\n\
        fun use(): String {\n\
        \x20 val a = configure<C>(strict = true)\n\
        \x20 val b = configure<C>(enabled = true, strict = true)\n\
        \x20 return a.hi() + b.hi()\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against_ref("cp_reified_named_default_after", LIB, MAIN)
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
    const MAIN: &str = "import lib.configure\n\
        class C {\n\
        \x20 var tag: String = \"\"\n\
        }\n\
        class Harness {\n\
        \x20 private val plain = configure<C> { tag = \"!\" }\n\
        \x20 private val named = configure<C>(enabled = true) { tag = \"!\" }\n\
        \x20 fun use(): String = plain.tag + named.tag\n\
        }\n\
        fun use(): String {\n\
        \x20 val plain = configure<C> { tag = \"!\" }\n\
        \x20 val named = configure<C>(enabled = true) { tag = \"!\" }\n\
        \x20 return plain.tag + named.tag\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against_ref("cp_reified_trailing_lambda", LIB, MAIN)
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
// after it preserves the same defaults-on-both-sides geometry and trailing receiver-lambda default.
const RUNTIME_LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    fun <T : Any> mkd(\n\
    \x20 seed: T,\n\
    \x20 label: String? = null,\n\
    \x20 enabled: Boolean = false,\n\
    \x20 vararg extraTypes: KClass<*>,\n\
    \x20 strict: Boolean = false,\n\
    \x20 block: T.() -> Unit = {},\n\
    ): T {\n\
    \x20 require(extraTypes.isEmpty())\n\
    \x20 if (enabled && label == null && !strict) block(seed)\n\
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
        \x20 val named = mkd(C(), enabled = true)\n\
        \x20 if (named.tag != \"\") return \"F2:\" + named.tag\n\
        \x20 val tagged = mkd(C(), enabled = true) { tag = \"!\" }\n\
        \x20 if (tagged.tag != \"!\") return \"F3:\" + tagged.tag\n\
        \x20 val after = mkd(C(), strict = true) { tag = \"?\" }\n\
        \x20 if (after.tag != \"\") return \"F4:\" + after.tag\n\
        \x20 return \"OK\"\n\
        }\n";
    if let Some(out) =
        common::expect_box_run_against_ref("cp_named_default_vararg_box", RUNTIME_LIB, MAIN)
    {
        assert_eq!(out, "OK");
    }
}
