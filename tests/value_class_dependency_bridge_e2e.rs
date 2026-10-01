//! A suspend override whose result is a dependency's value class (`Result`) replaces a generic
//! `T` result, so kotlinc writes the erased `execute(Continuation)` bridge to it. krusty derived
//! bridges before it knew which dependency classes are value classes, so it wrote none and the
//! interface call failed with `AbstractMethodError`.

use super::common;

const SRC: &str = "interface Gen<T> { suspend fun execute(): T }\n\
    class Impl(val r: Result<String>) : Gen<Result<String>> {\n\
    \x20   override suspend fun execute(): Result<String> = r\n\
    }\n";

#[test]
fn a_dependency_value_class_override_gets_its_bridge() {
    let built = common::compare_with_kotlinc_plugin(
        "DependencyValueClassBridge",
        SRC,
        "Impl",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let bridge = "public java.lang.Object execute(kotlin.coroutines.Continuation)";
    let reference = common::method_instructions(&built.reference, bridge);
    assert!(!reference.is_empty(), "kotlinc writes the bridge");
    assert_eq!(
        common::method_instructions(&built.krusty, bridge),
        reference
    );
}
