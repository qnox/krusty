//! An invisible candidate is reported when no later scope-tower level resolves the use.
//!
//! A constructor or member property the use site cannot see yields to a later visible candidate
//! (the box regressions `invisibleConstructorYieldsToCompanionInvoke.kt`,
//! `invisibleMemberPropertyYieldsToExtension.kt` and `invisibleJavaFieldYieldsToExtensionProperty.kt`
//! pin that). When nothing later resolves it, kotlinc reports the invisible candidate's access.
//! These pins keep that report exact once the candidate no longer ends the tower.

use super::common;

#[test]
fn an_invisible_candidate_with_no_later_level_is_reported_like_kotlinc() {
    const MAIN: &str = "class Plain private constructor()\n\
class Valued private constructor() { companion object }\n\
fun rejected() {\n\
    Plain()\n\
    Valued()\n\
    java.nio.ByteBuffer.allocate(1).hb\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "invisible candidates without a later level");
}
