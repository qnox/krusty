//! Unsigned values crossing a CLASSPATH call boundary, where the two sides disagree about whether
//! the value is BOXED.
//!
//! krusty carries `UInt`/`ULong` in the JVM primitive slot of their carrier (`I`/`J`) and boxes to
//! `kotlin/UInt`/`kotlin/ULong` only where a reference is required. Every classpath call is therefore
//! a place where the representation the lowerer produced has to agree with the descriptor the
//! backend spells verbatim. Two shapes disagreed and emitted a class that failed JVM verification
//! while krusty reported success:
//!
//! - `maxOf(a, b)` selects the value-class-mangled `maxOf-J1ME1BU:(II)I`, whose descriptor carries
//!   the ERASED carrier — but the parameter was recovered as the boxed metadata class, so the
//!   argument coercion boxed the carrier into a slot that takes it unboxed;
//! - `a.equals(b)` is an `invokevirtual` on `kotlin/UInt`, which needs the BOXED receiver — but the
//!   receiver was pushed as the raw carrier.
//!
//! `jvm_can_emit` could not catch either: it inspects the TYPES a file mentions, and `kotlin/UInt`
//! is a fully supported type there. What was wrong was the representation of a value at a call
//! boundary, which no signature-level check can see.
//!
//! Both now emit kotlinc's shapes, and `a.equals(b)` no longer reaches the member call at ANY argument
//! type — a same-type argument folds to a carrier compare (kotlinc's intrinsic) and every other one
//! goes to the static `equals-impl`, whose receiver slot is the carrier. Both shapes are pinned in
//! `bytecode_parity_e2e`; what
//! `unsigned_equals_keeps_value_class_semantics_across_argument_types` exercises here is that the
//! rerouting did not change what `equals` ANSWERS across the argument types.
//!
//! The contract pinned here is the backend's, not the feature's. Declining the file is always a
//! legal outcome; claiming success and writing a class that fails verification is not. So
//! [`expect_emitted_box_verifies`] accepts a backend decline and fails only when krusty says it
//! emitted a class and that class does not load and run.

use super::common;
use super::common::BackendOutcome;

/// Assert the backend's own contract: whatever krusty EMITS must verify and run.
///
/// A decline (any lowering/backend bail) passes — an unsupported construct is allowed to make the
/// file skip. Only "krusty emitted a class file, and the JVM rejects it" fails. A front-end
/// rejection fails too: these are backend tests and must not pass through a parse/type error.
fn expect_emitted_box_verifies(src: &str, stem: &str) {
    let stdlib = common::stdlib_jar();
    expect_emitted_box_verifies_on(src, stem, std::slice::from_ref(&stdlib));
}

/// [`expect_emitted_box_verifies`] against an explicit classpath — for a shape that needs a callee
/// no stdlib declaration has (a `suspend` classpath function with a defaulted unsigned parameter).
fn expect_emitted_box_verifies_on(src: &str, stem: &str, cp: &[std::path::PathBuf]) {
    let jdk = common::jdk_modules();
    match common::backend_outcome_in_process(src, stem, cp, Some(&jdk)) {
        None => panic!("{stem}: the front end rejected the source; this is a backend test"),
        Some(BackendOutcome::Emitted) => {
            let Some(out) = common::compile_and_run_box(src, stem, cp, Some(&jdk)) else {
                panic!(
                    "{stem}: krusty reported success, but the emitted class does not load and run \
                     (a class that fails verification is strictly worse than declining the file)"
                );
            };
            assert_eq!(out, "OK", "{stem}");
        }
        // Declined: the file skips. Always allowed.
        Some(_) => {}
    }
}

/// `maxOf` on an unsigned pair — the mangled `maxOf-J1ME1BU:(II)I` / `maxOf-eb3DHEI:(JJ)J`.
#[test]
fn unsigned_max_of_emits_verifiable_bytecode() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 200u\n\
    val b: UInt = 100u\n\
    return if (maxOf(a, b) == a) \"OK\" else \"bad\"\n\
}\n",
        "UMaxOf",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 200uL\n\
    val b: ULong = 100uL\n\
    return if (maxOf(a, b) == a) \"OK\" else \"bad\"\n\
}\n",
        "ULongMaxOf",
    );
}

/// The `minOf` sibling — same selection, same erased descriptor.
#[test]
fn unsigned_min_of_emits_verifiable_bytecode() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 200u\n\
    val b: UInt = 100u\n\
    return if (minOf(a, b) == b) \"OK\" else \"bad\"\n\
}\n",
        "UMinOf",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 200uL\n\
    val b: ULong = 100uL\n\
    return if (minOf(a, b) == b) \"OK\" else \"bad\"\n\
}\n",
        "ULongMinOf",
    );
}

/// `a.equals(b)`. A same-type pair folds to a carrier compare (kotlinc's intrinsic); any other
/// argument reaches the static `kotlin/UInt."equals-impl":(ILjava/lang/Object;)Z`, whose receiver
/// slot is the carrier — so neither path boxes the receiver. Both shapes are pinned in
/// `bytecode_parity_e2e`; both are exercised here for what they ANSWER.
#[test]
fn unsigned_equals_call_emits_verifiable_bytecode() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 200u\n\
    val b: UInt = 100u\n\
    if (a.equals(b)) return \"bad ne\"\n\
    if (!a.equals(a)) return \"bad eq\"\n\
    return \"OK\"\n\
}\n",
        "UEquals",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 200uL\n\
    val b: ULong = 100uL\n\
    if (a.equals(b)) return \"bad ne\"\n\
    if (!a.equals(a)) return \"bad eq\"\n\
    return \"OK\"\n\
}\n",
        "ULongEquals",
    );
}

/// A value class's member, reached on a receiver that HOLDS an unsigned value.
///
/// `kotlin/Result`'s members are mangled `-impl` statics whose descriptor spells the receiver as the
/// LEADING parameter (`getOrNull-impl:(Ljava/lang/Object;)Ljava/lang/Object;`) while the receiver
/// travels beside the arguments — the count mismatch that used to skip the check outright, and the
/// single most common one in the box corpus. The receiver slot is a reference here, so the boxed
/// `UInt` inside belongs in it and the call must EMIT; the alignment is what tells the two apart.
///
/// The payload is only tested for presence: comparing it back (`r.getOrNull() == 7u`) answers `false`
/// today, a separate wrong-VALUE defect in the boxed round trip that has nothing to do with which
/// descriptor slot the receiver occupies.
#[test]
fn value_class_impl_member_on_an_unsigned_payload_still_emits() {
    let stdlib = common::stdlib_jar();
    let cp = std::slice::from_ref(&stdlib);
    let jdk = common::jdk_modules();
    let src = "fun box(): String {\n\
    val r: Result<UInt> = Result.success(7u)\n\
    return if (r.getOrNull() != null) \"OK\" else \"bad\"\n\
}\n";
    assert_eq!(
        common::backend_outcome_in_process(src, "UResultImplMember", cp, Some(&jdk)),
        Some(BackendOutcome::Emitted),
        "the receiver occupies the descriptor's leading REFERENCE slot; do not decline"
    );
    expect_emitted_box_verifies(src, "UResultImplMember");
}

/// A BOXED unsigned argument to a `suspend` classpath function with a defaulted parameter.
///
/// The shape the descriptor/argument check used to skip outright. A `$default` synthetic spells the
/// CPS `Continuation` BEFORE the `int mask` + `Object marker`
/// (`libAny$default(Object, String, Continuation, int, Object)`) and the backend appends that
/// continuation at emit time, so the descriptor carries five slots where lowering built four values.
/// The counts disagreed and every argument went unchecked.
///
/// `7u` into an `Any` parameter is a legitimate box — `Ljava/lang/Object;` takes it — so this pins
/// that the shape is ALIGNED rather than merely skipped or blanket-declined: dropping the
/// continuation slot puts the box back on the reference slot it really occupies.
///
/// Needs a fixture jar: no stdlib `suspend` function pairs a default with a parameter that boxes.
#[test]
fn boxed_unsigned_argument_to_a_suspend_default_classpath_call_still_emits() {
    let stdlib = common::stdlib_jar();
    let Some(lib) = common::compile_libs(
        "USuspendDefaultLib",
        &[(
            "Lib.kt",
            "package lib\n\
fun mark(): String = \"!\"\n\
suspend fun libAny(t: Any, s: String = mark()): String = \"$t$s\"\n",
        )],
    ) else {
        // kotlinc unavailable in this environment: the fixture cannot be built, and a source-only
        // stand-in would not exercise a CLASSPATH call at all. A fixture kotlinc REJECTS panics
        // inside the helper rather than skipping.
        return;
    };
    let src = "import kotlin.coroutines.*\n\
import lib.libAny\n\
\n\
fun box(): String {\n\
    var out = \"\"\n\
    val body: suspend () -> Unit = { out = libAny(7u) }\n\
    body.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
    return if (out == \"7!\") \"OK\" else \"bad: $out\"\n\
}\n";
    let cp = [lib, stdlib];
    let jdk = common::jdk_modules();
    // STRICTER than the file's usual contract on purpose. A decline would pass there, and a decline
    // is exactly what a blanket-conservative answer to the count mismatch produces — the box is on
    // the stack, so "cannot align, therefore refuse" would swallow this legal call.
    assert_eq!(
        common::backend_outcome_in_process(src, "USuspendDefaultBoxed", &cp, Some(&jdk)),
        Some(BackendOutcome::Emitted),
        "a box landing in a reference slot must still emit once the continuation slot is aligned out"
    );
    expect_emitted_box_verifies_on(src, "USuspendDefaultBoxed", &cp);
}

/// An unsigned VALUE PARAMETER mangles the `suspend` function's JVM name (`libU` → `libU-OzbTU-A`),
/// and the `$default` synthetic is named from the mangled form. krusty looks suspend-ness up under
/// that JVM name. Suspend-ness therefore has to come from the metadata declaration selected by JVM
/// name AND descriptor shape, alongside the call facts already projected from that declaration.
/// Keyed on the source name alone it missed the mangled method, while indexing every suspend source
/// name would leak the flag to an ordinary overload. On the miss, the callable came back non-suspend,
/// the coroutine pass never threaded the `Continuation` the descriptor still spells, and the emitted
/// `invokestatic` was one argument short — a class that links and fails verification, which is the
/// outcome this whole file exists to rule out.
///
/// BOTH call forms are covered: the `$default` synthetic (an argument omitted) and the plain mangled
/// method (every argument supplied) missed the same way, so the fix cannot key on `$default`. Both
/// must EMIT and produce the callee's real answer — a decline would hide a silent regression back to
/// the name-keyed miss.
#[test]
fn mangled_suspend_classpath_call_threads_its_continuation() {
    let stdlib = common::stdlib_jar();
    let Some(lib) = common::compile_libs(
        "UMangledSuspendLib",
        &[(
            "Lib.kt",
            "package lib\n\
import kotlin.coroutines.Continuation\n\
fun mark(): String = \"!\"\n\
fun libU(c: Continuation<Unit>): String = \"plain\"\n\
suspend fun libU(t: UInt, s: String = mark()): String = \"$t$s\"\n",
        )],
    ) else {
        // kotlinc unavailable: the fixture cannot be built, and no stdlib declaration has this shape.
        // A fixture kotlinc REJECTS panics inside the helper rather than skipping.
        return;
    };
    let cp = [lib, stdlib];
    let jdk = common::jdk_modules();
    let body = |call: &str| {
        format!(
            "import kotlin.coroutines.*\n\
import lib.libU\n\
\n\
fun box(): String {{\n\
    var out = \"\"\n\
    val body: suspend () -> Unit = {{ out = {call} }}\n\
    body.startCoroutine(Continuation(EmptyCoroutineContext) {{ it.getOrThrow() }})\n\
    // The source-name sibling is NOT suspend. A name-wide flag would incorrectly classify it from\n\
    // the mangled declaration above and strip its ordinary trailing Continuation parameter, making\n\
    // this legal call unresolvable. The metadata fact must follow the selected JVM name and\n\
    // descriptor rather than leak across an overload family.\n\
    val plain = libU(Continuation(EmptyCoroutineContext) {{ _ -> }})\n\
    return if (out == \"7!\" && plain == \"plain\") \"OK\" else \"bad: $out/$plain\"\n\
}}\n"
        )
    };
    for (stem, call) in [
        // `libU-OzbTU-A$default(int, String, Continuation, int, Object)` — 5 slots, 4 values.
        ("UMangledSuspendDefault", "libU(7u)"),
        // `libU-OzbTU-A(int, String, Continuation)` — 3 slots, 2 values. No `$default` involved.
        ("UMangledSuspendPlain", "libU(7u, \"!\")"),
    ] {
        let src = body(call);
        assert_eq!(
            common::backend_outcome_in_process(&src, stem, &cp, Some(&jdk)),
            Some(BackendOutcome::Emitted),
            "{stem}: the mangled JVM name names a suspend function; thread its continuation"
        );
        // Not just verifiable: the coroutine has to actually run and hand back the callee's string,
        // which is what proves the continuation reached the callee rather than merely filling a slot.
        assert_eq!(
            common::compile_and_run_box(&src, stem, &cp, Some(&jdk)),
            Some("OK".to_string()),
            "{stem}"
        );
    }
}

/// The unthreaded-continuation decline keys on the UNFILLED slot, not on the word `Continuation`.
///
/// A non-suspend function may take one as an ordinary parameter, and a call to it — through the
/// `$default` synthetic or not — fills every slot it has. Refusing on the type alone would decline a
/// perfectly ordinary call, so both call forms are pinned as EMITTED.
#[test]
fn a_plain_continuation_parameter_is_not_an_unthreaded_continuation() {
    let stdlib = common::stdlib_jar();
    let Some(lib) = common::compile_libs(
        "ContParamDefaultLib",
        &[(
            "Lib.kt",
            "package lib\n\
import kotlin.coroutines.Continuation\n\
fun mark(): String = \"!\"\n\
fun libCont(c: Continuation<Unit>, s: String = mark()): String = \"$s\"\n",
        )],
    ) else {
        return;
    };
    let cp = [lib, stdlib];
    let jdk = common::jdk_modules();
    for (stem, call, want) in [
        // Through `libCont$default(Continuation, String, int, Object)` — 4 values over 4 slots.
        ("ContParamDefault", "libCont(c)", "!"),
        // The plain method `libCont(Continuation, String)` — 2 over 2.
        ("ContParamPlain", "libCont(c, \"x\")", "x"),
    ] {
        let src = format!(
            "import kotlin.coroutines.*\n\
import lib.libCont\n\
\n\
fun box(): String {{\n\
    val c = Continuation<Unit>(EmptyCoroutineContext) {{ }}\n\
    return if ({call} == \"{want}\") \"OK\" else \"bad\"\n\
}}\n"
        );
        assert_eq!(
            common::backend_outcome_in_process(&src, stem, &cp, Some(&jdk)),
            Some(BackendOutcome::Emitted),
            "{stem}: a declared Continuation parameter fills its own slot; do not decline"
        );
        expect_emitted_box_verifies_on(&src, stem, &cp);
    }
}

/// `UByte`/`UShort` have no carrier `Ty` of their own and are declined by `jvm_can_emit`'s
/// unsupported-value-class list. Pinned so the decline stays a DECLINE if that list ever moves —
/// the same shapes must never start emitting instead.
#[test]
fn narrow_unsigned_types_stay_declined_or_correct() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UByte = 200.toUByte()\n\
    val b: UByte = 100.toUByte()\n\
    return if (maxOf(a, b) == a) \"OK\" else \"bad\"\n\
}\n",
        "UByteMaxOf",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UShort = 200.toUShort()\n\
    val b: UShort = 100.toUShort()\n\
    return if (a.equals(b)) \"bad\" else \"OK\"\n\
}\n",
        "UShortEquals",
    );
}

/// The `equals` intrinsic must not change what `equals` MEANS. kotlinc folds only the same-type case
/// to a carrier compare; every other argument still goes through the value class's own equality —
/// `equals-impl`, which tests the argument's runtime class first. That is what makes a cross-carrier
/// comparison answer `false` (a `UInt` is never a `ULong`, even when the carriers hold the same bits),
/// a `null` argument answer `false`, and a boxed-but-equal one answer `true`.
#[test]
fn unsigned_equals_keeps_value_class_semantics_across_argument_types() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 4294967295u\n\
    val b: UInt = 1u\n\
    if (a.equals(b)) return \"f same-ne\"\n\
    if (!a.equals(a)) return \"f same-eq\"\n\
    if (a.equals(4294967295uL)) return \"f cross-carrier\"\n\
    val anyA: Any = a\n\
    if (!a.equals(anyA)) return \"f any-eq\"\n\
    if (a.equals(\"4294967295\")) return \"f any-ne\"\n\
    val na: UInt? = a\n\
    if (!a.equals(na)) return \"f nullable-eq\"\n\
    if (a.equals(null)) return \"f null\"\n\
    val i: Int = -1\n\
    if (a.equals(i)) return \"f signed-carrier\"\n\
    return \"OK\"\n\
}\n",
        "UEqualsSemantics",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 18446744073709551615uL\n\
    val b: ULong = 1uL\n\
    if (a.equals(b)) return \"f same-ne\"\n\
    if (!a.equals(a)) return \"f same-eq\"\n\
    val anyA: Any = a\n\
    if (!a.equals(anyA)) return \"f any-eq\"\n\
    val na: ULong? = a\n\
    if (!a.equals(na)) return \"f nullable-eq\"\n\
    val l: Long = -1L\n\
    if (a.equals(l)) return \"f signed-carrier\"\n\
    return \"OK\"\n\
}\n",
        "ULongEqualsSemantics",
    );
    // The narrow pair carries the same contract on its own `B`/`S` carrier.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UByte = 200.toUByte()\n\
    val anyA: Any = a\n\
    if (!a.equals(anyA)) return \"f any-eq\"\n\
    if (a.equals(200.toUShort())) return \"f cross-carrier\"\n\
    return \"OK\"\n\
}\n",
        "UByteEqualsSemantics",
    );
}

/// A member call whose receiver passed through a REFERENCE position in the source before reaching it.
///
/// These are the shapes that would double-box if the receiver-boxing branch ever saw an already-boxed
/// receiver: the value is boxed while it sits in the nullable local / map / elvis, so a branch that
/// boxed it again would push a `Lkotlin/UInt;` at the `(I)` its own factory declares.
///
/// They do NOT reach the member call boxed today, and this test does not pretend otherwise. Checked
/// against the emitted bytecode: `UNullableBangEquals`, `UMapValueEquals`, `UWhenReceiverEquals` and
/// `ULongNullableBangEquals` emit, and each one UNBOXES to the carrier (the `!!`/erased-read coercion)
/// and then boxes once for the `invokevirtual` — the round trip is visible as
/// `checkcast kotlin/UInt; unbox-impl; box-impl`. `UNullableSmartCastEquals`, `UNullableSafeEquals`
/// and `UElvisReceiverEquals` are declined by the backend outright.
///
/// So what is pinned here is the backend's contract, not the representation query's arms: these are
/// the source shapes closest to putting a boxed unsigned in receiver position, and whatever the
/// lowerer decides about any of them, the result must be a class that verifies and runs — or a
/// decline. The query's own arms are covered where they can be reached directly, by the walk's unit
/// tests in `src/ir_lower.rs`.
#[test]
fn unsigned_member_calls_on_a_reference_carried_receiver_verify() {
    // A nullable local: the value lives boxed, and `!!` brings it back to a member call.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val u: UInt? = 5u\n\
    val s: Any = \"5\"\n\
    return if (!u!!.equals(s)) \"OK\" else \"bad\"\n\
}\n",
        "UNullableBangEquals",
    );
    // The same value reached by a smart cast rather than by an assertion.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val u: UInt? = 5u\n\
    val s: Any = \"5\"\n\
    if (u != null) return if (!u.equals(s)) \"OK\" else \"bad\"\n\
    return \"bad null\"\n\
}\n",
        "UNullableSmartCastEquals",
    );
    // A safe call — the receiver is a temp the lowerer introduced, not a source local.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val u: UInt? = 5u\n\
    val s: Any = \"5\"\n\
    return if (u?.equals(s) == false) \"OK\" else \"bad\"\n\
}\n",
        "UNullableSafeEquals",
    );
    // An ERASED read: the map holds the box, and the element comes back as `Object`.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val m = mapOf(\"k\" to 5u)\n\
    val s: Any = \"5\"\n\
    return if (!m[\"k\"]!!.equals(s)) \"OK\" else \"bad\"\n\
}\n",
        "UMapValueEquals",
    );
    // A `when` receiver — no single node produces the value the call runs on.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 5u\n\
    val b: UInt = 7u\n\
    val s: Any = \"5\"\n\
    val c = a < b\n\
    return if (!(if (c) a else b).equals(s)) \"OK\" else \"bad\"\n\
}\n",
        "UWhenReceiverEquals",
    );
    // An elvis result, then a member call on it.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val u: UInt? = null\n\
    val s: Any = \"0\"\n\
    return if (!(u ?: 0u).equals(s)) \"OK\" else \"bad\"\n\
}\n",
        "UElvisReceiverEquals",
    );
    // The `ULong` carrier takes the same path through its own box.
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val u: ULong? = 5uL\n\
    val s: Any = \"5\"\n\
    return if (!u!!.equals(s)) \"OK\" else \"bad\"\n\
}\n",
        "ULongNullableBangEquals",
    );
}

/// An unsigned RECEIVER of an inline-spliced scope function (`5u.let { … }`).
///
/// After #531 a spliced lambda parameter is typed as the semantic wrapper (`kotlin/UInt`), so the
/// splice entry `checkcast`s to that inline class and the body unboxes through it. The value filling
/// that parameter therefore has to be a real `box-impl` instance. It was not: the splice's own
/// argument loop boxes the host's scalar operands and cannot see unsignedness — `5u` reaches the
/// emitter as `Const(Int(5))` — so it emitted `Integer.valueOf` and the shape threw
/// `ClassCastException` at run time while krusty reported success.
///
/// The emitter has no way to arrange agreement: nothing there maps a host argument onto the lambda
/// parameter it feeds. It can only rule the splice out, so this shape now DECLINES, which is why it
/// is pinned with this file's permissive helper rather than a strict emit-and-run one.
///
/// The real fix belongs in lowering, which does know the type: an unsigned argument flowing into a
/// reference parameter already gets its `box-impl` from `coerce_argument_value` (that is why
/// `listOf(5u)` produces a genuine `kotlin/UInt`), but the RECEIVER of an inline classpath extension
/// never reaches that coercion. Until it does, declining is the only sound outcome here.
///
/// `listOf(5u).map { … }` is deliberately NOT affected: it passes a `List` reference, so the splice
/// boxes no scalar and the element still arrives from `Iterator.next()` already boxed.
#[test]
fn unsigned_receiver_of_an_inline_scope_function_never_miscompiles() {
    expect_emitted_box_verifies(
        "fun box(): String = if (5u.let { it.toString() } == \"5\") \"OK\" else \"bad\"\n",
        "UIntLetReceiver",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val d: ULong = 5uL\n\
    return if (d.let { it.toString() } == \"5\") \"OK\" else \"bad\"\n\
}\n",
        "ULongLetReceiver",
    );
    // A carrier whose signed reading differs, so a wrong box that VERIFIES is caught too.
    expect_emitted_box_verifies(
        "fun f(): UInt = 4294967295u\n\
         fun box(): String =\n\
    if (f().let { it.toString() } == \"4294967295\") \"OK\" else \"bad\"\n",
        "UIntLetCallReceiver",
    );
}
