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

// A zero-argument generic producer has nothing to bind `T` from on its own, so its type comes from
// the enclosing member parameter. The explicit-argument form is the independent control: a written
// type argument must bind the producer's return regardless of surrounding context.
const MATCHER_LIB: &str = "package lib\n\
    import kotlin.reflect.KClass\n\
    class Payload\n\
    class GenericSource {\n\
    \x20 inline fun <reified T : Any> provide(): T = TODO()\n\
    \x20 fun <T : Any> provide(classifier: KClass<T>): T = TODO()\n\
    }\n\
    class Sink {\n\
    \x20 fun accept(payload: Payload): String = \"x\"\n\
    \x20 suspend fun persist(payload: Payload): String = \"x\"\n\
    \x20 suspend fun pair(left: String, right: String) {}\n\
    }\n\
    class ExactSource {\n\
    \x20 fun provide(): Any = Any()\n\
    }\n\
    class DivergentSink {\n\
    \x20 fun accept(payload: Payload): String = \"payload\"\n\
    \x20 fun accept(text: String): String = \"text\"\n\
    }\n\
    fun <T> within(block: suspend GenericSource.() -> T): T = TODO()\n";

#[test]
fn zero_arg_generic_member_binds_from_argument_position() {
    const MAIN: &str = "import lib.GenericSource\n\
        import lib.Sink\n\
        import lib.within\n\
        fun t(sink: Sink, source: GenericSource) {\n\
        \x20 within { sink.persist(provide()) }\n\
        \x20 within { sink.pair(provide(), provide()) }\n\
        \x20 val direct: String = sink.accept(source.provide())\n\
        \x20 direct.length\n\
        }\n";
    let Some(diagnostics) =
        common::checker_diags_against_ref("member_matcher_arg", MATCHER_LIB, MAIN)
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
    const MAIN: &str = "import lib.GenericSource\n\
        import lib.Payload\n\
        import lib.Sink\n\
        fun t(sink: Sink, source: GenericSource) {\n\
        \x20 val v = source.provide<Payload>()\n\
        \x20 val r: String = sink.accept(v)\n\
        \x20 val inline1: String = sink.accept(source.provide<Payload>())\n\
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

#[test]
fn expected_type_retry_requires_bindable_return_and_overload_agreement() {
    // Negative boundary: a declaration that genuinely returns `Any` cannot be retyped merely because
    // an enclosing parameter wants a narrower class. Likewise, two applicable parameter mappings that
    // disagree provide no authoritative expectation for an unbound generic producer.
    const MAIN: &str = "import lib.DivergentSink\n\
        import lib.ExactSource\n\
        import lib.GenericSource\n\
        import lib.Sink\n\
        fun bad(exact: ExactSource, generic: GenericSource) {\n\
        \x20 Sink().accept(exact.provide())\n\
        \x20 DivergentSink().accept(generic.provide())\n\
        }\n";
    let Some(diagnostics) = common::checker_diags_against(
        "member_expected_type_negative_boundaries",
        MATCHER_LIB,
        MAIN,
    ) else {
        return;
    };
    assert!(
        diagnostics.len() >= 2,
        "exact Any and divergent expectations must both remain rejected, got {diagnostics:?}"
    );
}

// The RUNTIME half of expected-type inference: `take(): T` really returns `Object` on the JVM,
// so once the checker records the bound type, the LOWERING must reconcile the erased producer
// with the enclosing parameter's descriptor — only the run output can prove it.
const MATCHER_RUNTIME_LIB: &str = "package lib\n\
    class Payload(val name: String)\n\
    class GenericCell {\n\
    \x20 var value: Any? = null\n\
    \x20 @Suppress(\"UNCHECKED_CAST\")\n\
    \x20 fun <T : Any> read(): T = value as T\n\
    }\n\
    class Sink {\n\
    \x20 fun label(payload: Payload): String = \"payload:\" + payload.name\n\
    }\n";

#[test]
fn zero_arg_generic_member_argument_box_runs() {
    const MAIN: &str = "import lib.GenericCell\n\
        import lib.Payload\n\
        import lib.Sink\n\
        fun box(): String {\n\
        \x20 val cell = GenericCell()\n\
        \x20 cell.value = Payload(\"sample\")\n\
        \x20 val got = Sink().label(cell.read())\n\
        \x20 return if (got == \"payload:sample\") \"OK\" else \"F:\" + got\n\
        }\n";
    if let Some(out) =
        common::expect_box_run_against("member_matcher_arg_box", MATCHER_RUNTIME_LIB, MAIN)
    {
        assert_eq!(out, "OK");
    }
}
