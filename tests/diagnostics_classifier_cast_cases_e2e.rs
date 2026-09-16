//! Exact classifier, hierarchy-cycle, and cast diagnostic cases against kotlinc.

use super::diagnostics_parity_support::assert_error_parity;

#[test]
fn classifier_cycle_and_cast_errors_match_kotlinc_exactly() {
    let cases = [
        // A deliberately unique type present on NO supplied classpath. Keep the spelling synthetic:
        // diagnostic regressions must not depend on or disclose a class from the scanned project.
        "fun f(p: DefinitelyAbsentClassifier): Int = 0",
        // Declaration bounds use the same normal classifier resolution and must fail before the
        // metadata encoder. This source previously reached emission and panicked there.
        "abstract class C<P>(val p: P) where P : DefinitelyAbsentBoundA, P : DefinitelyAbsentBoundB",
        // A cyclic source hierarchy reports the frontend error at the same source coordinate rather
        // than reaching an unguarded inheritance walker.
        "object DefinitelyCyclicClassifier : DefinitelyCyclicClassifier()",
        // The diagnostic belongs to the edge that participates in the cycle, not an innocent
        // earlier supertype in the same declaration.
        "interface MixedCycle : InnocentSupertype, CyclicPeer\ninterface InnocentSupertype\ninterface CyclicPeer : MixedCycle",
        // An `is` whose TARGET type is unresolved reports the unresolved reference at the type's
        // span — never a compiler-specific "not supported" rejection.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier",
        // … and a failing type ARGUMENT is named at its own span, not the outer generic's.
        "fun f(p: Any) = p is Array<DefinitelyAbsentClassifier>",
        // Ordinary generic arguments retain an outer `Ty::Obj`; nested Error detection must inspect
        // that semantic shape instead of relying on `outer == Ty::Error`.
        "fun f(p: Any) = p is List<DefinitelyAbsentClassifier>",
        // When the OUTER name is the unresolvable one it is named first, not its type argument.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier<String>",
        // A nullable unresolved target resolves to the same reference diagnostic.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier?",
        // The `as` sibling reports identically.
        "fun f(p: Any) = p as DefinitelyAbsentClassifier",
        "fun f(p: Any) = p as List<DefinitelyAbsentClassifier>",
        "fun f(p: Any) = p is (DefinitelyAbsentClassifier) -> String",
        "fun f(p: Any) = p is Function1<DefinitelyAbsentClassifier, String>",
        "fun f(p: Any) = p is Function1<Any?, Any?>",
        "fun f(p: Any) = p is Array",
        "fun f(p: Any) = p is Array<Nothing>",
        "fun f(p: Any) = p as Array",
        "fun f(p: Any) = p as Array<Nothing>",
        // Two unrelated final classifiers have no possible runtime overlap, so the cast is an
        // error rather than bytecode that can only fail. Keep this in exact kotlinc parity coverage.
        "fun box(): String { val s = 1 as String; return s }",
        // Casts permit an erased function shape, so an unresolved parameter remains the primary
        // diagnostic. (`is` is different and is pinned by the unsupported-shape test below.)
        "fun f(p: Any) = p as (DefinitelyAbsentClassifier) -> String",
    ];

    assert_error_parity(&cases);
}
