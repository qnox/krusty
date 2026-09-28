//! A class delegating an interface whose suspend member returns a value class forwards to the
//! member by its suspend JVM name (`foo-vneLvdU`), as kotlinc does. The forwarder is not a state
//! machine: it passes its continuation through, returns `COROUTINE_SUSPENDED` unchanged, and
//! `checkcast`s any other result to the carrier.

use super::common;

const SRC: &str = "@JvmInline value class Tag(val s: String)\n\
    interface Source {\n\
    \x20   suspend fun tag(s: String): Tag\n\
    }\n\
    class Plain : Source {\n\
    \x20   override suspend fun tag(s: String): Tag = Tag(s)\n\
    }\n\
    class Forward(val source: Source) : Source by source\n";

#[test]
fn a_delegated_suspend_member_is_called_by_its_suspend_name() {
    let built = common::compare_with_kotlinc_plugin(
        "SuspendValueClassDelegation",
        SRC,
        "Forward",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public java.lang.Object tag-";
    let reference = forwarder_shape(&common::method_instructions(&built.reference, method));
    assert!(
        reference
            .iter()
            .any(|instruction| instruction.contains("invokeinterface")),
        "kotlinc forwards through the interface call"
    );
    assert_eq!(
        forwarder_shape(&common::method_instructions(&built.krusty, method)),
        reference
    );
}

/// The delegate field's simple name is not the calling convention. kotlinc reuses the constructor
/// property; the synthesized storage field is a separate fact. Program counters, the suspend name,
/// and the carrier adapter stay exact.
fn forwarder_shape(instructions: &[String]) -> Vec<String> {
    instructions
        .iter()
        .map(|instruction| {
            let Some((head, field)) = instruction.split_once("// Field ") else {
                return instruction.clone();
            };
            let Some((_, descriptor)) = field.split_once(':') else {
                return instruction.clone();
            };
            format!("{head}// Field {descriptor}")
        })
        .collect()
}
