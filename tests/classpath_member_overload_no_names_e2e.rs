//! A classpath member OVERLOAD PAIR where one overload records no parameter names — the
//! mockk shape: `inline fun <reified T : Any> any(): T` next to `fun <T : Any> any(classifier:
//! KClass<T>): T`. The zero-parameter overload's empty name list made the slot-mapping path drop
//! it WITHOUT deferring to the direct member path, so the sibling overload's mapping error
//! rejected the whole call: every `any()` inside a mockk `every { }` block reported
//! "no value passed for parameter 'classifier'". The zero-arg call must resolve cleanly; the
//! `classifier` overload stays callable explicitly.
use super::common;

const LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    class Scope {\n\
    \x20 val seen = mutableListOf<String>()\n\
    \x20 fun <T : Any> pick(classifier: KClass<T>): T {\n\
    \x20   seen.add(classifier.simpleName ?: \"?\")\n\
    \x20   @Suppress(\"UNCHECKED_CAST\") return \"x\" as T\n\
    \x20 }\n\
    \x20 inline fun <reified T : Any> pick(): T = pick(T::class)\n\
    }\n\
    fun scope(block: Scope.() -> Unit): Scope {\n\
    \x20 val s = Scope()\n\
    \x20 s.block()\n\
    \x20 return s\n\
    }\n";

#[test]
fn zero_arg_overload_resolves_next_to_named_sibling() {
    // Frontend-only assertion: the reified body's SPLICE may legitimately decline (a backend
    // bail, not a diagnostic) — the checker must accept the call either way.
    const MAIN: &str = "import lib.Scope\n\
        fun t(s: Scope) {\n\
        \x20 val v: String = s.pick()\n\
        \x20 val w: String = s.pick(String::class)\n\
        \x20 v.length + w.length\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against("member_no_names", LIB, MAIN) else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "zero-arg overload must resolve cleanly next to its named sibling"
    );
}

#[test]
fn zero_arg_overload_resolves_inside_receiver_lambda() {
    // The mockk call shape: the zero-arg overload called through the block's implicit receiver.
    const MAIN: &str = "import lib.scope\n\
        fun t() {\n\
        \x20 scope {\n\
        \x20   val v: String = pick()\n\
        \x20   v.length\n\
        \x20 }\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against("member_no_names_lambda", LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "zero-arg overload must resolve through the receiver lambda"
    );
}

// The INVERSE guard: deferring to the direct path because a no-names sibling exists must not
// starve a LABELLED call that only the names-bearing overload can answer. The direct member
// fallback binds known labels positionally, so an over-eager deferral is not just a spurious
// diagnostic — with same-typed parameters it silently swaps the arguments at runtime.
const LABELLED_LIB: &str = "package lib\n\
    class Tagger {\n\
    \x20 fun tag(prefix: String, value: Int): String = prefix + value\n\
    \x20 fun tag(): String = \"none\"\n\
    }\n\
    class Swapper {\n\
    \x20 fun join(first: String, second: String): String = first + \"|\" + second\n\
    \x20 fun join(): String = \"none\"\n\
    }\n";

#[test]
fn reordered_named_args_resolve_next_to_no_names_sibling() {
    // Positional binding would put `value = 3` (Int) into `prefix` (String); only slot mapping
    // against the names-bearing overload accepts this call, exactly as kotlinc does.
    const MAIN: &str = "import lib.Tagger\n\
        fun t(p: Tagger) {\n\
        \x20 val a: String = p.tag(value = 3, prefix = \"v\")\n\
        \x20 a.length\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against("member_no_names_reorder", LABELLED_LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "reordered named args must slot-map despite the zero-param sibling"
    );
}

#[test]
fn reordered_same_type_named_args_keep_label_order_at_runtime() {
    // Same-typed parameters: a positional rebinding type-checks by coincidence, so only the
    // runtime output can prove the labels were honored.
    const MAIN: &str = "import lib.Swapper\n\
        fun box(): String {\n\
        \x20 val r = Swapper().join(second = \"B\", first = \"A\")\n\
        \x20 return if (r == \"A|B\") \"OK\" else \"FAIL:\" + r\n\
        }\n\
        fun main() { println(box()) }\n";
    common::expect_box_ok_against("member_no_names_swap", LABELLED_LIB, MAIN);
}

// The mockk MATCHER-IN-ARGUMENT shape: `coEvery { client.createOrganization(any()) }`. The
// zero-arg reified `any()` has NOTHING to bind `T` from on its own — its type must come from the
// ENCLOSING member call's parameter (`org: Org`), exactly as kotlinc's expected-type inference
// binds it. Includes the explicit-argument form `any<Org>()`, whose written type argument must
// bind the member's return regardless of surrounding context.
const MATCHER_LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    class Org\n\
    class Matcher {\n\
    \x20 inline fun <reified T : Any> any(): T = TODO()\n\
    \x20 fun <T : Any> any(classifier: KClass<T>): T = TODO()\n\
    }\n\
    class Client {\n\
    \x20 fun sync(org: Org): String = \"x\"\n\
    \x20 suspend fun create(org: Org): String = \"x\"\n\
    \x20 suspend fun add(orgId: String, userId: String) {}\n\
    }\n\
    fun <T> ev(block: suspend Matcher.() -> T): T = TODO()\n";

#[test]
fn zero_arg_generic_member_binds_from_argument_position() {
    const MAIN: &str = "import lib.Client\n\
        import lib.Matcher\n\
        import lib.ev\n\
        fun t(c: Client, m: Matcher) {\n\
        \x20 ev { c.create(any()) }\n\
        \x20 ev { c.add(any(), any()) }\n\
        \x20 val direct: String = c.sync(m.any())\n\
        \x20 direct.length\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against("member_matcher_arg", MATCHER_LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "a zero-arg generic member argument must bind from the enclosing parameter type"
    );
}

#[test]
fn explicit_type_argument_binds_generic_member_return() {
    const MAIN: &str = "import lib.Client\n\
        import lib.Matcher\n\
        import lib.Org\n\
        fun t(c: Client, m: Matcher) {\n\
        \x20 val v = m.any<Org>()\n\
        \x20 val r: String = c.sync(v)\n\
        \x20 val inline1: String = c.sync(m.any<Org>())\n\
        \x20 r.length + inline1.length\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against("member_explicit_targ", MATCHER_LIB, MAIN)
    else {
        return;
    };
    assert_eq!(
        diagnostics,
        Vec::<String>::new(),
        "an explicit type argument must bind a generic member's return"
    );
}

// The RUNTIME half of expected-type inference: `take(): T` really returns `Object` on the JVM,
// so once the checker records the bound type, the LOWERING must reconcile the erased producer
// with the enclosing parameter's descriptor — only the run output can prove it.
const MATCHER_RUNTIME_LIB: &str = "package lib\n\
    class Org(val name: String)\n\
    class Holder {\n\
    \x20 var value: Any? = null\n\
    \x20 @Suppress(\"UNCHECKED_CAST\")\n\
    \x20 fun <T : Any> take(): T = value as T\n\
    }\n\
    class Client {\n\
    \x20 fun label(org: Org): String = \"org:\" + org.name\n\
    }\n";

#[test]
fn zero_arg_generic_member_argument_box_runs() {
    const MAIN: &str = "import lib.Client\n\
        import lib.Holder\n\
        import lib.Org\n\
        fun box(): String {\n\
        \x20 val h = Holder()\n\
        \x20 h.value = Org(\"acme\")\n\
        \x20 val got = Client().label(h.take())\n\
        \x20 return if (got == \"org:acme\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) =
        common::expect_box_run_against("member_matcher_arg_box", MATCHER_RUNTIME_LIB, MAIN)
    {
        assert_eq!(out, "OK");
    }
}
