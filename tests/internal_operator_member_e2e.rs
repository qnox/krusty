//! An `internal` member of another module is invisible at the call site, so an applicable
//! extension takes the operator call, the indexed read and the call, as for a private member.
//!
//! This is the whole original program, which constructs the dependency class in the main module.
//! kotlinc runs it first and its `box()` result is the oracle. The KLIB backends do not yet
//! construct a class another module declares, so the portable box regression
//! `tests/box_regressions/multimodule/internalOperatorMemberThroughFactoryYieldsToExtension.kt`
//! reaches the same calls through a library function instead.
use super::common;

const LIB: &str = "\
package lib
class B {
    internal operator fun plus(x: Int): String = \"fail: member plus\"
    internal operator fun get(index: Int): String = \"fail: member get\"
    internal fun tag(): String = \"fail: member tag\"
}
";

const MAIN: &str = "\
import lib.B
operator fun B.plus(x: Int): String = \"O\"
operator fun B.get(index: Int): String = \"K\"
fun B.tag(): String = \"\"
fun box(): String = (B() + 1) + B()[0] + B().tag()
";

#[test]
fn an_internal_member_of_a_constructed_dependency_class_yields_to_extensions_like_kotlinc() {
    let library = common::kotlinc_library(LIB).expect("kotlinc builds the dependency module");
    let reference = common::kotlinc_box_result_with_classpath(MAIN, std::slice::from_ref(&library));
    assert_eq!(reference, "OK", "kotlinc runs the original program");
    assert_eq!(
        common::expect_box_run_against("internal_operator_member", LIB, MAIN).as_deref(),
        Some(reference.as_str()),
    );
}
