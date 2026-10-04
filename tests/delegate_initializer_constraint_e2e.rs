//! A builder lambda collects constraints from initialized local statements even when their
//! bindings are not the block result. KT-65262 (`pclaRootIsTrySyntheticCallWithDelegate`) reads a
//! delegated property only from an `if` condition; kotlinc still evaluates the initializer for
//! inference.

use super::common;

const SOURCE: &str = r#"interface ValueSink<in T>
fun <T> buildValue(block: (ValueSink<T>) -> Unit): T = "OK" as T
fun requireTextSink(x: ValueSink<String>) {}
class DeferredCell<E>(val value: E)
fun <L> deferValue(initializer: () -> L): DeferredCell<L> = DeferredCell(initializer())
operator fun <G> DeferredCell<G>.getValue(thisRef: Any?, property: Any?): G = value
class SinkDelegate<S>(val sink: ValueSink<S>, val value: S)
fun <S> delegateFor(sink: ValueSink<S>): SinkDelegate<S> = SinkDelegate(sink, "OK" as S)
operator fun <S> SinkDelegate<S>.getValue(thisRef: Any?, property: Any?): S = value
fun usedInCondition() = buildValue { x ->
    val y by deferValue { requireTextSink(x); "OK" }
    if (y.length != 2) error("fail")
}
fun unusedDelegate() = buildValue { x ->
    val y by deferValue { requireTextSink(x); "OK" }
}
fun typedDelegate() = buildValue { x ->
    val y: String by delegateFor(x)
}
fun ordinaryInitializer() = buildValue { x ->
    val ignored = requireTextSink(x)
}
fun usedAsResult() = buildValue { x ->
    val y by deferValue { requireTextSink(x); "OK" }
    y
}
fun inTry() = try {
    buildValue { x ->
        val y by deferValue { requireTextSink(x); "OK" }
        if (y.length != 2) error("fail")
    }
} finally {
}
fun box(): String {
    if (usedInCondition() != "OK") return "condition"
    if (unusedDelegate() != "OK") return "unused delegate"
    if (typedDelegate() != "OK") return "typed delegate"
    if (ordinaryInitializer() != "OK") return "ordinary initializer"
    if (usedAsResult() != "OK") return "result"
    if (inTry() != "OK") return "try"
    return "OK"
}
"#;

/// Each expression-bodied function returns `String`: a statement initializer or the declared
/// delegated-property result fixes the builder type variable even when the local is unused.
#[test]
fn local_initializers_fix_the_builder_type_argument() {
    common::assert_class_code_matches_kotlinc(
        "DelegateInitializerConstraint",
        SOURCE,
        "DelegateInitializerConstraintKt",
    );
    common::expect_box_same_as_kotlinc(SOURCE, "DelegateInitializerConstraintRun");
}
