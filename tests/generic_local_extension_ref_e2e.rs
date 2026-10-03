//! A generic local extension participates in a callable reference after its receiver is
//! specialized through the symbol hierarchy and its formal bounds hold. Source order still
//! decides visibility: a reference written before the local function binds the same-named
//! extension property. The classifiers here are repository-owned and invariant.

use super::common::{
    assert_error_blocks_match_kotlinc, expect_box_run_against, expect_box_run_against_kotlinc,
    expect_box_run_against_ref, expect_box_same_as_kotlinc, kotlinc_box_result_with_classpath,
    kotlinc_library,
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
fn bound_and_unbound_references_choose_the_nearest_declared_receiver() {
    expect_box_same_as_kotlinc(
        r#"
open class ReceiverBase
class ReceiverDerived : ReceiverBase()

fun box(): String {
    fun ReceiverBase.pick(): String = "base"
    fun ReceiverDerived.pick(): String = "derived"

    val value = ReceiverDerived()
    val bound: () -> String = value::pick
    val unbound: (ReceiverDerived) -> String = ReceiverDerived::pick
    return if (bound() == "derived" && unbound(value) == "derived") "OK" else "fail"
}
"#,
        "LocalExtensionReferenceReceiverRank",
    );
}

#[test]
fn a_defaulted_local_extension_beats_an_equally_specific_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class AmbiguousReceiver

fun box(): String {
    fun AmbiguousReceiver.choose(value: Int = 1): Int = 10 + value
    fun AmbiguousReceiver.choose(vararg values: Int): Int = 20 + values.size
    val receiver = AmbiguousReceiver()
    val zero: () -> Int = receiver::choose
    val one: (Int) -> Int = receiver::choose
    val two: (Int, Int) -> Int = receiver::choose
    val unboundZero: (AmbiguousReceiver) -> Int = AmbiguousReceiver::choose
    val unboundOne: (AmbiguousReceiver, Int) -> Int = AmbiguousReceiver::choose
    val unboundTwo: (AmbiguousReceiver, Int, Int) -> Int = AmbiguousReceiver::choose
    val log = "${zero()}-${one(7)}-${two(3, 4)}/" +
        "${unboundZero(receiver)}-${unboundOne(receiver, 7)}-${unboundTwo(receiver, 3, 4)}"
    return if (log == "11-17-22/11-17-22") "OK" else "fail:$log"
}
"#,
        "LocalExtensionReferenceSpecificity",
    );
}

#[test]
fn a_defaulted_receiverless_local_beats_an_equally_specific_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class ReceiverlessLocalPiece(val text: String)

fun box(): String {
    fun choose(value: ReceiverlessLocalPiece = ReceiverlessLocalPiece("D")): String =
        "fixed:${value.text}"
    fun choose(vararg values: ReceiverlessLocalPiece): String =
        "vararg:${values.size}" +
            if (values.size == 2) ":${values[0].text}${values[1].text}" else ""
    val zero: () -> String = ::choose
    val one: (ReceiverlessLocalPiece) -> String = ::choose
    val two: (ReceiverlessLocalPiece, ReceiverlessLocalPiece) -> String = ::choose
    val log = "${zero()}-${one(ReceiverlessLocalPiece("A"))}-" +
        two(ReceiverlessLocalPiece("A"), ReceiverlessLocalPiece("B"))
    return if (log == "fixed:D-fixed:A-vararg:2:AB") "OK" else "fail:$log"
}
"#,
        "ReceiverlessLocalReferenceSpecificity",
    );
}

#[test]
fn an_unbound_local_context_extension_keeps_the_receiver_after_context_parameters() {
    expect_box_same_as_kotlinc(
        r#"
// LANGUAGE: +ContextParameters
class Prefix(val text: String)
class Target(val text: String)
class Piece(val text: String)

fun box(): String {
    context(prefix: Prefix)
    fun Target.join(first: Piece = Piece("D"), vararg rest: Piece): String =
        prefix.text + text + first.text + if (rest.size == 0) "" else rest[0].text

    val target = Target("T")
    val boundDefault: context(Prefix) () -> String = target::join
    val boundPair: context(Prefix) (Piece, Piece) -> String = target::join
    val unboundDefault: context(Prefix) (Target) -> String = Target::join
    val unboundPair: context(Prefix) (Target, Piece, Piece) -> String = Target::join
    val prefix = Prefix("P")
    val values =
        boundDefault(prefix) + boundPair(prefix, Piece("A"), Piece("B")) +
        unboundDefault(prefix, target) +
        unboundPair(prefix, target, Piece("A"), Piece("B"))
    return if (values == "PTDPTABPTDPTAB") "OK" else "fail:$values"
}
"#,
        "LocalContextExtensionReferenceAlignment",
    );
}

#[test]
fn a_defaulted_top_level_extension_beats_an_equally_specific_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class TopLevelReceiver

fun TopLevelReceiver.choose(value: Int = 1): Int = 10 + value
fun TopLevelReceiver.choose(vararg values: Int): Int = 20 + values.size

fun box(): String {
    val receiver = TopLevelReceiver()
    val zero: () -> Int = receiver::choose
    val one: (Int) -> Int = receiver::choose
    val two: (Int, Int) -> Int = receiver::choose
    val unboundZero: (TopLevelReceiver) -> Int = TopLevelReceiver::choose
    val unboundOne: (TopLevelReceiver, Int) -> Int = TopLevelReceiver::choose
    val unboundTwo: (TopLevelReceiver, Int, Int) -> Int = TopLevelReceiver::choose
    val log = "${zero()}-${one(7)}-${two(3, 4)}/" +
        "${unboundZero(receiver)}-${unboundOne(receiver, 7)}-${unboundTwo(receiver, 3, 4)}"
    return if (log == "11-17-22/11-17-22") "OK" else "fail:$log"
}
"#,
        "TopLevelExtensionReferenceSpecificity",
    );
}

#[test]
fn a_defaulted_receiverless_top_level_beats_an_equally_specific_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class ReceiverlessTopLevelPiece(val text: String)

fun choose(value: ReceiverlessTopLevelPiece = ReceiverlessTopLevelPiece("D")): String =
    "fixed:${value.text}"
fun choose(vararg values: ReceiverlessTopLevelPiece): String =
    "vararg:${values.size}" +
        if (values.size == 2) ":${values[0].text}${values[1].text}" else ""

fun box(): String {
    val zero: () -> String = ::choose
    val one: (ReceiverlessTopLevelPiece) -> String = ::choose
    val two: (ReceiverlessTopLevelPiece, ReceiverlessTopLevelPiece) -> String = ::choose
    val log = "${zero()}-${one(ReceiverlessTopLevelPiece("A"))}-" +
        two(ReceiverlessTopLevelPiece("A"), ReceiverlessTopLevelPiece("B"))
    return if (log == "fixed:D-fixed:A-vararg:2:AB") "OK" else "fail:$log"
}
"#,
        "ReceiverlessTopLevelReferenceSpecificity",
    );
}

#[test]
fn a_defaulted_constructor_beats_an_equally_specific_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class ConstructorPiece(val text: String)

class Constructed(val result: String) {
    constructor(value: ConstructorPiece = ConstructorPiece("D")) : this("fixed:${value.text}")
    constructor(vararg values: ConstructorPiece) : this(
        "vararg:${values.size}" +
            if (values.size == 2) ":${values[0].text}${values[1].text}" else ""
    )
}

fun box(): String {
    val zero: () -> Constructed = ::Constructed
    val one: (ConstructorPiece) -> Constructed = ::Constructed
    val two: (ConstructorPiece, ConstructorPiece) -> Constructed = ::Constructed
    val log = "${zero().result}-${one(ConstructorPiece("A")).result}-" +
        two(ConstructorPiece("A"), ConstructorPiece("B")).result
    return if (log == "fixed:D-fixed:A-vararg:2:AB") "OK" else "fail:$log"
}
"#,
        "ConstructorReferenceSpecificity",
    );
}

#[test]
fn a_defaulted_member_beats_an_equally_specific_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class MemberReceiver {
    fun choose(value: Int = 1): Int = 10 + value
    fun choose(vararg values: Int): Int = 20 + values.size
}

fun box(): String {
    val receiver = MemberReceiver()
    val zero: () -> Int = receiver::choose
    val one: (Int) -> Int = receiver::choose
    val two: (Int, Int) -> Int = receiver::choose
    val unboundZero: (MemberReceiver) -> Int = MemberReceiver::choose
    val unboundOne: (MemberReceiver, Int) -> Int = MemberReceiver::choose
    val unboundTwo: (MemberReceiver, Int, Int) -> Int = MemberReceiver::choose
    val log = "${zero()}-${one(7)}-${two(3, 4)}/" +
        "${unboundZero(receiver)}-${unboundOne(receiver, 7)}-${unboundTwo(receiver, 3, 4)}"
    return if (log == "11-17-22/11-17-22") "OK" else "fail:$log"
}
"#,
        "MemberReferenceSpecificity",
    );
}

#[test]
fn concrete_receiver_domain_beats_a_generic_receiver_domain() {
    expect_box_same_as_kotlinc(
        r#"
class Container<T>

fun box(): String {
    fun <T> Container<T>.pick(): String = "generic"
    fun Container<String>.pick(): String = "concrete"

    val value = Container<String>()
    val bound: () -> String = value::pick
    val unbound: (Container<String>) -> String = Container<String>::pick
    return if (bound() == "concrete" && unbound(value) == "concrete") "OK" else "fail"
}
"#,
        "LocalExtensionReferenceGenericReceiverSpecificity",
    );
}

#[test]
fn a_defaulted_local_extension_beats_an_empty_vararg() {
    expect_box_same_as_kotlinc(
        r#"
class Choice(val text: String)
class ChoiceReceiver

fun box(): String {
    fun ChoiceReceiver.choose(value: Choice = Choice("default")): String = value.text
    fun ChoiceReceiver.choose(vararg values: Choice): String = "vararg"

    val receiver = ChoiceReceiver()
    val selected: () -> String = receiver::choose
    return if (selected() == "default") "OK" else selected()
}
"#,
        "BoundLocalExtensionAdaptationCost",
    );
}

#[test]
fn equal_cost_unrelated_local_extension_shapes_are_ambiguous() {
    const SOURCE: &str = r#"class AmbiguousReceiver
class FirstInput
class SecondInput

fun probe(receiver: AmbiguousReceiver) {
    fun AmbiguousReceiver.choose(value: FirstInput = FirstInput()): String = "first"
    fun AmbiguousReceiver.choose(value: SecondInput = SecondInput()): String = "second"
    val selected: () -> String = receiver::choose
}
"#;

    assert_error_blocks_match_kotlinc(&[("LocalExtensionReferenceAmbiguity.kt", SOURCE)], &[]);
}

#[test]
fn receiverless_equal_default_shapes_remain_ambiguous() {
    const SOURCE: &str = r#"class FirstInput
class SecondInput

fun choose(value: FirstInput = FirstInput()): String = "first"
fun choose(value: SecondInput = SecondInput()): String = "second"

fun localProbe() {
    fun choose(value: FirstInput = FirstInput()): String = "first"
    fun choose(value: SecondInput = SecondInput()): String = "second"
    val selected: () -> String = ::choose
}

fun topLevelProbe() {
    val selected: () -> String = ::choose
}
"#;

    assert_error_blocks_match_kotlinc(&[("ReceiverlessReferenceAmbiguity.kt", SOURCE)], &[]);
}

#[test]
fn constructor_equal_default_shapes_remain_ambiguous() {
    const SOURCE: &str = r#"class ConstructorFirstInput
class ConstructorSecondInput

class AmbiguousConstructed {
    constructor(value: ConstructorFirstInput = ConstructorFirstInput()) {}
    constructor(value: ConstructorSecondInput = ConstructorSecondInput()) {}
}

fun constructorProbe() {
    val selected: () -> AmbiguousConstructed = ::AmbiguousConstructed
}
"#;

    assert_error_blocks_match_kotlinc(&[("ConstructorReferenceAmbiguity.kt", SOURCE)], &[]);
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

const PROVIDER_SPECIFICITY_LIB: &str = r#"
package provider

class ProviderReceiver
class ProviderPiece(val text: String)

fun ProviderReceiver.choose(value: Int = 1): Int = 10 + value
fun ProviderReceiver.choose(vararg values: Int): Int = 20 + values.size

fun select(value: ProviderPiece = ProviderPiece("D")): String = "fixed:${value.text}"
fun select(vararg values: ProviderPiece): String =
    "vararg:${values.size}" +
        if (values.size == 2) ":${values[0].text}${values[1].text}" else ""
"#;

const PROVIDER_SPECIFICITY_MAIN: &str = r#"
import provider.ProviderReceiver
import provider.ProviderPiece
import provider.choose
import provider.select

fun box(): String {
    val receiver = ProviderReceiver()
    val zero: () -> Int = receiver::choose
    val one: (Int) -> Int = receiver::choose
    val two: (Int, Int) -> Int = receiver::choose
    val unboundZero: (ProviderReceiver) -> Int = ProviderReceiver::choose
    val unboundOne: (ProviderReceiver, Int) -> Int = ProviderReceiver::choose
    val unboundTwo: (ProviderReceiver, Int, Int) -> Int = ProviderReceiver::choose
    val log = "${zero()}-${one(7)}-${two(3, 4)}/" +
        "${unboundZero(receiver)}-${unboundOne(receiver, 7)}-${unboundTwo(receiver, 3, 4)}"
    val rootZero: () -> String = ::select
    val rootOne: (ProviderPiece) -> String = ::select
    val rootTwo: (ProviderPiece, ProviderPiece) -> String = ::select
    val root = "${rootZero()}-${rootOne(ProviderPiece("A"))}-" +
        rootTwo(ProviderPiece("A"), ProviderPiece("B"))
    return if (log == "11-17-22/11-17-22" && root == "fixed:D-fixed:A-vararg:2:AB") "OK"
        else "fail:$log/$root"
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

#[test]
fn a_kotlinc_provider_uses_the_shared_adapted_reference_specificity() {
    let Some(reference_library) = kotlinc_library(PROVIDER_SPECIFICITY_LIB) else {
        return;
    };
    let reference =
        kotlinc_box_result_with_classpath(PROVIDER_SPECIFICITY_MAIN, &[reference_library]);
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    assert_eq!(
        expect_box_run_against(
            "provider_callable_ref_specificity",
            PROVIDER_SPECIFICITY_LIB,
            PROVIDER_SPECIFICITY_MAIN,
        )
        .as_deref(),
        Some(reference.as_str()),
    );
    assert_eq!(
        expect_box_run_against_kotlinc(PROVIDER_SPECIFICITY_LIB, PROVIDER_SPECIFICITY_MAIN)
            .as_deref(),
        Some(reference.as_str()),
    );
}
