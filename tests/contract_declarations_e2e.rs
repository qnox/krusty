//! Where a `contract { … }` may be declared and what its description may refer to.
//!
//! A contract is the first statement of a function's block body. kotlinc rejects one written
//! later or in a nested block, an `init` block or a lambda, one declared by a local function or an
//! open or overriding member, and one used as an expression body. Its description may refer only to
//! the owner's value parameters and extension receiver, and describes each parameter with at most
//! one `callsInPlace`. Property accessors and secondary constructors may declare one.
//!
//! A description call is an effect only when it selects a `ContractBuilder` or `SimpleEffect`
//! member, and an invocation kind only when it selects an entry of `InvocationKind`; a same-named
//! extension or a foreign `EXACTLY_ONCE` is reported, never read by its spelling. The contract call
//! itself declares a contract only when it is spelled `contract` and selects the intrinsic.
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

#[test]
fn a_description_call_is_an_effect_only_when_it_selects_the_dsl() {
    assert_matches_kotlinc(
        "fun ContractBuilder.callsInPlace(block: () -> Unit, times: Int): CallsInPlace =
    callsInPlace(block)
fun ContractBuilder.returnsNotNull(flag: Boolean): ReturnsNotNull = returnsNotNull()
infix fun Returns.implies(flag: String): ConditionalEffect = implies(true)

fun sameNamedEffect(block: () -> Unit) {
    contract { callsInPlace(block, 2) }
    block()
}
fun sameNamedSimpleEffect(x: Any?): Any? {
    contract { returnsNotNull(true) implies (x != null) }
    return x
}
fun sameNamedImplies(x: Any?): Boolean {
    contract { returns(true) implies \"s\" }
    return true
}
fun notDsl(x: Any?): Boolean {
    contract { x.toString() }
    return x is String
}
fun use(a: Any?): Int = if (sameNamedImplies(a)) a.length else 0
",
    );
}

#[test]
fn an_invocation_kind_is_an_entry_of_invocation_kind() {
    assert_matches_kotlinc(
        "val EXACTLY_ONCE: InvocationKind = InvocationKind.AT_MOST_ONCE
object Foreign {
    val EXACTLY_ONCE: InvocationKind = InvocationKind.AT_MOST_ONCE
}
fun topLevel(block: () -> Unit) {
    contract { callsInPlace(block, EXACTLY_ONCE) }
    block()
}
fun objectMember(block: () -> Unit) {
    contract { callsInPlace(block, Foreign.EXACTLY_ONCE) }
    block()
}
fun parameter(block: () -> Unit, kind: InvocationKind) {
    contract { callsInPlace(block, kind) }
    block()
}
",
    );
}

/// A same-package `contract` that accepts the builder lambda outranks the star-imported intrinsic,
/// so it declares no contract; an import alias of the intrinsic is not spelled `contract` and is a
/// misplaced contract.
#[test]
fn the_contract_call_is_identified_by_the_declaration_it_selects() {
    common::assert_errors_match_kotlinc(
        &[
            (
                "alias.kt",
                "@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
package alias
import kotlin.contracts.contract as declare

fun aliased(x: Any?): Boolean {
    declare { returns(true) implies (x is String) }
    return x is String
}
fun use(a: Any?): Int = if (aliased(a)) a.length else 0
",
            ),
            (
                "shadow.kt",
                "@file:OptIn(kotlin.contracts.ExperimentalContracts::class)
package shadow
import kotlin.contracts.*

fun contract(block: ContractBuilder.() -> Unit) {}

fun shadowed(x: Any?): Boolean {
    contract { returns(true) implies (x is String) }
    return x is String
}
fun use(a: Any?): Int = if (shadowed(a)) a.length else 0
",
            ),
        ],
        &[],
    );
}
