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
