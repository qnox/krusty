//! A suspend lambda whose parameter or receiver is a value class, as kotlinc writes its class.
//!
//! A suspend lambda is always a `SuspendLambda` class, never an `invokedynamic`, so nothing hands it
//! a value class through a generic slot except its own erased `FunctionN.invoke`. kotlinc therefore
//! lowers its body over each value class's carrier, as it lowers any function's: the typed `invoke`
//! takes the carrier and is mangled like a function taking a value class (`invoke-<hash>`, the
//! continuation hashed as `_`, its `Any?` result never), the erased bridge and `create` unbox the box
//! they receive (null-safely for a nullable reference carrier), the typed `invoke` boxes the carrier
//! again to hand it to `create`, and the parameter's spill field holds the carrier.
//!
//! krusty lowered the body over the box, as it does for a lambda an `invokedynamic` creates, and
//! named the typed `invoke` plainly, so its bridge called an `invoke` that did not exist.

use super::common;

/// Each listed class is written byte for byte as kotlinc writes it.
fn assert_classes_match_kotlinc(file: &str, source: &str, classes: &[&str]) {
    let built = common::classes_against_kotlinc_module(&[(file, source)]);
    let mismatches = classes
        .iter()
        .filter_map(|&class| {
            let reference = built
                .reference
                .get(class)
                .unwrap_or_else(|| panic!("kotlinc writes {class}"));
            let ours = built
                .krusty
                .get(class)
                .unwrap_or_else(|| panic!("krusty writes {class}"));
            (reference != ours).then(|| common::exact_class_difference(class, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

const PARAMETERS: &str = "import kotlin.coroutines.*\n\
@JvmInline value class IC(val s: String)\n\
@JvmInline value class BI(val v: Int)\n\
@JvmInline value class G<T>(val t: T)\n\
fun builder(c: suspend () -> Unit) {\n\
\x20   c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
fun box(): String {\n\
\x20   var res = \"\"\n\
\x20   val two: suspend (IC, IC) -> String = { a, b -> a.s + b.s }\n\
\x20   val receiver: suspend BI.() -> Unit = { res += v }\n\
\x20   val nullableReference: suspend (IC?) -> Unit = { res += it?.s }\n\
\x20   val nullableScalar: suspend (BI?) -> Unit = { res += it?.v }\n\
\x20   val generic: suspend (G<String>) -> Unit = { res += it.t }\n\
\x20   val unused: suspend (BI) -> Unit = { res += \"u\" }\n\
\x20   val nullableUnsigned: suspend (UInt?) -> Unit = { res += it }\n\
\x20   builder {\n\
\x20       res += two(IC(\"O\"), IC(\"K\"))\n\
\x20       BI(1).receiver()\n\
\x20       nullableReference(IC(\"r\"))\n\
\x20       nullableReference(null)\n\
\x20       nullableScalar(BI(2))\n\
\x20       nullableScalar(null)\n\
\x20       generic(G(\"g\"))\n\
\x20       unused(BI(3))\n\
\x20       nullableUnsigned(4u)\n\
\x20   }\n\
\x20   return if (res == \"OK1rnull2nullgu4\") \"OK\" else res\n\
}\n";

// `generic` runs below but is left out of the byte comparison: its body reads `it.t` off the
// `Object` carrier with a `checkcast String`, where kotlinc appends the `Object` itself, a
// difference of the generic value-class property read in any body, not of the lambda class.
#[test]
fn a_value_class_parameter_takes_its_carrier_through_a_mangled_typed_invoke() {
    assert_classes_match_kotlinc(
        "SuspendValueParameters.kt",
        PARAMETERS,
        &[
            "SuspendValueParametersKt$box$two$1",
            "SuspendValueParametersKt$box$receiver$1",
            "SuspendValueParametersKt$box$nullableReference$1",
            "SuspendValueParametersKt$box$nullableScalar$1",
            "SuspendValueParametersKt$box$unused$1",
            "SuspendValueParametersKt$box$nullableUnsigned$1",
        ],
    );
}

#[test]
fn a_value_class_parameter_runs_through_the_erased_bridge_and_create() {
    common::expect_box_ok_with_stdlib(PARAMETERS, "SuspendValueParameters");
}

/// A lambda whose parameter type is inferred from a generic suspend function type, `T := IC`.
const INFERRED: &str = "import kotlin.coroutines.*\n\
@JvmInline value class IC(val s: String)\n\
fun builder(c: suspend () -> Unit) {\n\
\x20   c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
suspend fun <T> List<T>.onEach(c: suspend (T) -> Unit) {\n\
\x20   for (e in this) c(e)\n\
}\n\
fun box(): String {\n\
\x20   var res = \"\"\n\
\x20   builder { listOf(IC(\"O\"), IC(\"K\")).onEach { res += it.s } }\n\
\x20   return res\n\
}\n";

#[test]
fn an_inferred_value_class_parameter_is_mangled_like_a_declared_one() {
    assert_classes_match_kotlinc(
        "SuspendInferred.kt",
        INFERRED,
        &["SuspendInferredKt$box$1$1"],
    );
    common::expect_box_ok_with_stdlib(INFERRED, "SuspendInferred");
}
