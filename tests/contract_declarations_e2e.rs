//! Where a `contract { … }` may be declared and what its description may refer to.
//!
//! A contract is the first statement of a function's block body. kotlinc rejects one written
//! later or in a nested block, an `init` block or a lambda, one declared by a local function or an
//! open or overriding member, and one used as an expression body. Its description may refer only to
//! the owner's value parameters and extension receiver, and describes each parameter with at most
//! one `callsInPlace`. Property accessors and secondary constructors may declare one.
//!
//! DIFFERENTIAL: krusty's errors are compared with kotlinc's, complete and in order.

use super::common;

const PRELUDE: &str = "@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
import kotlin.contracts.*
";

fn assert_matches_kotlinc(body: &str) {
    let source = format!("{PRELUDE}\n{body}");
    common::assert_errors_match_kotlinc(&[("main.kt", &source)], &[]);
}

#[test]
fn a_contract_is_the_first_statement_of_a_function_block_body() {
    assert_matches_kotlinc(
        "fun later(v: Any?) {
    val w = v
    contract { returns() implies (v != null) }
}
fun nested(v: Any?, c: Boolean) {
    if (c) { contract { returns() implies (v != null) } }
}
fun twice(v: Any?) {
    contract { returns() implies (v != null) }
    contract { returns() implies (v != null) }
}
class Init(v: Any?) {
    init { contract { returns() implies (v != null) } }
}
val lambda = { v: Any? -> contract { returns() implies (v != null) } }
fun expressionBody(v: Any?) = contract { returns() implies (v != null) }
class Allowed(val p: Any?) {
    val accessor: Int
        get() {
            contract { returns() }
            return 1
        }
    constructor(q: Int) : this(null) {
        contract { returns() }
    }
}
",
    );
}

#[test]
fn local_open_and_overriding_functions_cannot_declare_a_contract() {
    assert_matches_kotlinc(
        "interface Api {
    fun inInterface(v: Any?) { contract { returns() implies (v != null) } }
    fun abstractOne(v: Any?)
}
abstract class Base : Api {
    final override fun abstractOne(v: Any?) { contract { returns() implies (v != null) } }
    open fun openOne(v: Any?) { contract { returns() implies (v != null) } }
    private fun privateOne(v: Any?) { contract { returns() implies (v != null) } }
}
fun outer() {
    fun local(v: Any?) { contract { returns() implies (v != null) } }
}
",
    );
}

#[test]
fn a_description_refers_only_to_parameters_and_the_extension_receiver() {
    assert_matches_kotlinc(
        "val global: Any? = null
fun notParameter() {
    contract { returns() implies (global != null) }
}
class Holder(val property: Any?) {
    fun member() {
        contract { returns() implies (property != null) }
    }
}
fun thisWithoutReceiver() {
    contract { returns() implies (this != null) }
}
fun Any?.receiver(): Boolean {
    contract { returns(true) implies (this@receiver != null) }
    return this != null
}
fun notReference(v: Any?) {
    contract { callsInPlace(v as () -> Unit) }
}
fun inPlaceTwice(block: () -> Unit) {
    contract {
        callsInPlace(block, InvocationKind.EXACTLY_ONCE)
        callsInPlace(block, InvocationKind.AT_MOST_ONCE)
    }
    block()
}
",
    );
}
