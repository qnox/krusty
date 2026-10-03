//! A generic local extension participates in a callable reference after its receiver is
//! specialized through the symbol hierarchy and its formal bounds hold. Source order still
//! decides visibility: a reference written before the local function binds the same-named
//! extension property. The classifiers here are repository-owned and invariant.

use super::common::{
    expect_box_run_against_kotlinc, expect_box_run_against_ref, expect_box_same_as_kotlinc,
};

#[test]
fn a_later_generic_local_extension_wins_an_unbound_reference() {
    expect_box_same_as_kotlinc(
        r#"
class Items<T>

var result = "failed"

val <T> Items<T>.foo: T
    get() = "not" as T

val <T> Items<T>.bar: T.() -> String
    get() = { "fai" }

val <T> (Items<T>.() -> T).baz: T
    get() = this(Items())

fun box(): String {
    fun <T> test(): Items<T>.() -> T = Items<T>::foo
    result = test<String>()(Items())

    fun <T> test2(): Items<T>.() -> (T.() -> String) = Items<T>::bar
    result += test2<String>()(Items())("")

    fun <T> Items<T>.foo(): T { return "led" as T }
    fun <T> test3(): () -> T = Items<T>::foo::baz
    result += test3<String>()()

    return if (result == "notfailed") "OK" else result
}
"#,
        "GenericLocalExtensionRef",
    );
}

#[test]
fn a_generic_local_extension_wins_a_bound_reference() {
    expect_box_same_as_kotlinc(
        r#"
class Items<T>(val value: T)

val <T> Items<T>.item: String
    get() = "prop"

fun box(): String {
    fun <T> Items<T>.item(): String = "ext"
    val xs = Items("a")
    val read: () -> String = xs::item
    return if (read() == "ext") "OK" else read()
}
"#,
        "BoundGenericLocalExtensionRef",
    );
}

#[test]
fn an_earlier_reference_still_binds_the_extension_property() {
    expect_box_same_as_kotlinc(
        r#"
class Items<T>(val value: T)

val <T> Items<T>.item: String
    get() = "prop"

fun box(): String {
    val xs = Items("a")
    val read: () -> String = xs::item
    fun <T> Items<T>.item(): String = "ext"
    return if (read() == "prop") "OK" else read()
}
"#,
        "EarlierExtensionPropertyRef",
    );
}

#[test]
fn a_derived_receiver_specializes_a_base_local_extension() {
    expect_box_same_as_kotlinc(
        r#"
open class Base<T>
class Derived<U> : Base<U>()

fun box(): String {
    fun <T> Base<T>.pick(): String = "base"
    val value = Derived<String>()
    val bound: () -> String = value::pick
    fun <T> unbound(): Derived<T>.() -> String = Derived<T>::pick
    val through = unbound<String>()
    return if (bound() == "base" && through(value) == "base") "OK" else bound() + through(value)
}
"#,
        "DerivedBaseLocalExtensionRef",
    );
}

#[test]
fn a_bound_violation_keeps_the_extension_property() {
    expect_box_same_as_kotlinc(
        r#"
interface Marker
class Box<T>(val value: T)
class Plain

val <T> Box<T>.pick: String
    get() = "prop"

fun box(): String {
    fun <T : Marker> Box<T>.pick(): String = "ext"
    val box = Box(Plain())
    val read: () -> String = box::pick
    return if (read() == "prop") "OK" else read()
}
"#,
        "BoundedLocalExtensionPropertyFallback",
    );
}

#[test]
fn an_expectation_free_reference_exposes_the_specialized_receiver() {
    expect_box_same_as_kotlinc(
        r#"
open class Base<T>(val value: T)

fun box(): String {
    fun <T> Base<T>.pick(): T = value
    val unbound = Base<String>::pick
    val bound = Base("ok")::pick
    return if (unbound(Base("u")) == "u" && bound() == "ok") "OK" else "fail"
}
"#,
        "ExpectationFreeLocalExtensionRef",
    );
}

#[test]
fn receiver_qualified_local_extensions_adapt_for_function_and_sam_expectations() {
    expect_box_same_as_kotlinc(
        r#"
class Counter(val seed: String)
class Piece(val text: String)

fun interface BoundZero { fun dispatch(): String }
fun interface BoundPair { fun dispatch(first: Piece, second: Piece): String }
fun interface UnboundZero { fun dispatch(counter: Counter): String }
fun interface UnboundPair {
    fun dispatch(counter: Counter, first: Piece, second: Piece): String
}

fun consumeBoundFunction(action: () -> String): String = action()
fun consumeBoundPairFunction(action: (Piece, Piece) -> String): String =
    action(Piece("A"), Piece("B"))
fun consumeUnboundFunction(action: (Counter) -> String, counter: Counter): String = action(counter)
fun consumeUnboundPairFunction(
    action: (Counter, Piece, Piece) -> String,
    counter: Counter,
): String = action(counter, Piece("A"), Piece("B"))
fun consumeBoundZero(action: BoundZero): String = action.dispatch()
fun consumeBoundPair(action: BoundPair): String = action.dispatch(Piece("A"), Piece("B"))
fun consumeUnboundZero(action: UnboundZero, counter: Counter): String = action.dispatch(counter)
fun consumeUnboundPair(action: UnboundPair, counter: Counter): String =
    action.dispatch(counter, Piece("A"), Piece("B"))

fun box(): String {
    fun Counter.accumulate(first: Piece = Piece("D"), vararg remaining: Piece): String =
        seed + first.text + if (first.text == "A") remaining[0].text else ""

    val counter = Counter("S")
    val boundDefault: () -> String = counter::accumulate
    val boundVararg: (Piece, Piece) -> String = counter::accumulate
    val unboundDefault: (Counter) -> String = Counter::accumulate
    val unboundVararg: (Counter, Piece, Piece) -> String = Counter::accumulate

    val values =
        boundDefault() + boundVararg(Piece("A"), Piece("B")) +
        unboundDefault(counter) + unboundVararg(counter, Piece("A"), Piece("B")) +
        consumeBoundFunction(counter::accumulate) + consumeBoundPairFunction(counter::accumulate) +
        consumeUnboundFunction(Counter::accumulate, counter) +
        consumeUnboundPairFunction(Counter::accumulate, counter) +
        consumeBoundZero(counter::accumulate) + consumeBoundPair(counter::accumulate) +
        consumeUnboundZero(Counter::accumulate, counter) +
        consumeUnboundPair(Counter::accumulate, counter)
    return if (values == "SDSABSDSABSDSABSDSABSDSABSDSAB") "OK" else "fail:$values"
}
"#,
        "LocalExtensionReferenceExpectedShapes",
    );
}

#[test]
fn callable_references_continue_to_an_applicable_outer_extension_rung() {
    expect_box_same_as_kotlinc(
        r#"
class OuterReceiver
class InnerReceiver

fun box(): String {
    fun OuterReceiver.choose(): String = "outer"

    fun nested(): String {
        fun InnerReceiver.choose(): String = "inner"

        val value = OuterReceiver()
        val direct = value.choose()
        val bound: () -> String = value::choose
        val unbound: (OuterReceiver) -> String = OuterReceiver::choose
        return direct + bound() + unbound(value)
    }

    return if (nested() == "outerouterouter") "OK" else "fail"
}
"#,
        "LocalExtensionReferenceLexicalRungs",
    );
}

#[test]
fn a_bound_reference_selects_the_cheapest_local_extension_adaptation() {
    expect_box_same_as_kotlinc(
        r#"
class Choice(val text: String)
class ChoiceReceiver

fun box(): String {
    fun ChoiceReceiver.choose(value: Choice = Choice("default")): String = value.text
    fun ChoiceReceiver.choose(vararg values: Choice): String = "vararg"

    val receiver = ChoiceReceiver()
    val selected: () -> String = receiver::choose
    return if (selected() == "vararg") "OK" else selected()
}
"#,
        "BoundLocalExtensionAdaptationCost",
    );
}

const LIB: &str = r#"
package lib
open class Base<T>
class Derived : Base<String>()
"#;

const MAIN: &str = r#"
import lib.Base
import lib.Derived

fun box(): String {
    fun <T> Base<T>.pick(): String = "base"
    val bound = Derived()::pick
    val unbound = Derived::pick
    return if (bound() == "base" && unbound(Derived()) == "base") "OK" else "fail"
}
"#;

#[test]
fn a_classpath_derived_receiver_specializes_a_base_local_extension() {
    assert_eq!(
        expect_box_run_against_ref("local_ext_hierarchy", LIB, MAIN).as_deref(),
        Some("OK")
    );
}

#[test]
fn a_kotlinc_derived_receiver_specializes_a_base_local_extension() {
    assert_eq!(
        expect_box_run_against_kotlinc(LIB, MAIN).as_deref(),
        Some("OK")
    );
}
