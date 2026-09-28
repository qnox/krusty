//! A class delegating an interface whose suspend member returns a value class forwards to the
//! member by its suspend JVM name (`foo-vneLvdU`), as kotlinc does. krusty named the forwarded call
//! once as a suspend function and then again as an ordinary one (`foo-vneLvdU-JEFnHOQ`), a method
//! no class declares.

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
    let calls = |disassembly: &str| {
        common::method_instructions(disassembly, method)
            .into_iter()
            .filter(|instruction| instruction.contains("invokeinterface"))
            .collect::<Vec<_>>()
    };
    let reference = calls(&built.reference);
    assert_eq!(
        reference.len(),
        1,
        "kotlinc forwards through one interface call"
    );
    assert_eq!(calls(&built.krusty), reference);
}
