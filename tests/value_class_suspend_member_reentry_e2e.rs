//! A value class's suspend member is realized as a static `-impl` method of the class, and the
//! continuation of its state machine re-enters it there, as kotlinc does. krusty called the
//! re-entry on the file facade, which declares no such method.

use super::common;

const SRC: &str = "suspend fun echo(s: String): String = s\n\
    @JvmInline value class Tag(val s: String) {\n\
    \x20   suspend fun twice(): Tag = Tag(echo(s) + echo(s))\n\
    }\n";

#[test]
fn a_value_class_suspend_member_is_reentered_on_its_class() {
    let built = common::compare_with_kotlinc_plugin(
        "SuspendMemberReentry",
        SRC,
        "Tag$twice$1",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public final java.lang.Object invokeSuspend(java.lang.Object)";
    let reentries = |disassembly: &str| {
        common::method_instructions(disassembly, method)
            .into_iter()
            .filter(|instruction| instruction.contains("invokestatic"))
            .take(1)
            .collect::<Vec<_>>()
    };
    let reference = reentries(&built.reference);
    assert_eq!(reference.len(), 1, "kotlinc re-enters the member once");
    assert_eq!(reentries(&built.krusty), reference);
}
