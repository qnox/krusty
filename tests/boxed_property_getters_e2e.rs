//! A getter whose result maps to a JVM scalar where an overridden getter returns a reference
//! returns the wrapper, as a function's result does (see `boxed_override_results_e2e.rs`): `override val level:
//! Int` of `Gauge<T>.level: T` is `getLevel()Ljava/lang/Integer;` with a `getLevel()Object` bridge,
//! and over `val count: Int?` it is `getCount()Ljava/lang/Integer;` with no bridge at all. A
//! source-written getter, a `super` read, a delegation forwarder, a caller in another file and an
//! override of a kotlinc-compiled dependency all agree on the wrapper.

use super::common;

const DECLARATIONS: &str = "interface Gauge<T> { val level: T }
open class Meter : Gauge<Int> { override val level: Int = 3 }
interface Holder { val count: Int? }
open class Fixed(override val count: Int) : Holder
class Forward(gauge: Gauge<Int>) : Gauge<Int> by gauge
";

const OVERRIDES: &str = "class Raised : Meter() { override val level: Int get() = super.level + 1 }
class Counted(start: Int) : Fixed(start) { override val count: Int get() = super.count + 1 }
fun read(meter: Meter, raised: Raised, fixed: Fixed, counted: Counted): Int =
    meter.level + raised.level + fixed.count + counted.count
fun box(): String {
    val gauge: Gauge<Int> = Raised()
    val forward: Gauge<Int> = Forward(Meter())
    if (read(Meter(), Raised(), Fixed(2), Counted(1)) != 11) return \"read\"
    if (gauge.level != 4 || forward.level != 3) return \"bridge\"
    return \"OK\"
}
";

#[test]
fn a_boxed_property_getter_matches_kotlinc_across_files() {
    let sources = [
        ("Declarations.kt", DECLARATIONS),
        ("Overrides.kt", OVERRIDES),
    ];
    for class in [
        "Gauge",
        "Holder",
        "Meter",
        "Fixed",
        "Forward",
        "Raised",
        "Counted",
        "OverridesKt",
    ] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert!(pair.krusty == pair.kotlinc, "{class} differs from kotlinc");
    }
}

#[test]
fn a_boxed_property_getter_runs() {
    let source = format!("{DECLARATIONS}{OVERRIDES}");
    assert_eq!(
        common::expect_box_run_with_stdlib(&source, "BoxedPropertyGetters"),
        "OK"
    );
}

const TWO_SLOTS: &str = "open class Sized<T> { open var size: T = 56 as T }
interface Counted { var size: Int }
open class Both : Counted, Sized<Int>()
open class Own : Both() { override var size: Int = 117 }
";

/// An override over both an `Int` slot and a type parameter's slot keeps one bridge per slot: the
/// boxed getter bridges to `getSize()I` and to `getSize()Object`, and the setter, whose own
/// descriptor already matches the `Int` slot, bridges only to `setSize(Object)`.
#[test]
fn a_boxed_getter_over_two_slots_bridges_to_each_like_kotlinc() {
    let sources = [("TwoSlots.kt", TWO_SLOTS)];
    let pair = common::ModuleClassPair::compile(&sources, "Own");
    assert!(pair.krusty == pair.kotlinc, "Own differs from kotlinc");
}

const LIBRARY: &str = "package lib
interface Gauge<T> { val level: T }
open class Meter : Gauge<Int> { override val level: Int = 3 }
interface Holder { val count: Int? }
open class Fixed(override val count: Int) : Holder
";

const DEPENDENT: &str = "import lib.*
class Raised : Meter() { override val level: Int get() = super.level + 1 }
class Counted(start: Int) : Fixed(start) { override val count: Int get() = super.count + 1 }
fun read(meter: Meter, raised: Raised, counted: Counted): Int = meter.level + raised.level + counted.count
fun box(): String = if (read(Meter(), Raised(), Counted(1)) == 9) \"OK\" else \"fail\"
";

/// kotlinc's dependency getter already returns the wrapper: an override keeps that descriptor, and
/// bridges only to the type parameter's erasure.
#[test]
fn an_override_of_a_boxed_dependency_getter_matches_kotlinc() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIBRARY, DEPENDENT)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
    let classes =
        common::classes_against_kotlinc_lib("Dependent", &[("Lib.kt", LIBRARY)], DEPENDENT)
            .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const FUN_INTERFACES: &str = "interface Base { fun f(): Any }
fun interface Child : Base { override fun f(): Int }
fun wrap(h: () -> Int): Int = Child(h).f()
fun lambda(): Int = Child { 2 }.f()
";

/// A fun interface's scalar result over a reference-returning overridden method is decided by the
/// same JVM rule: the wrapper class around a function value returns the wrapper and bridges to the
/// overridden slot exactly as kotlinc's does, and a lambda's class implements the boxed result.
#[test]
fn a_fun_interface_over_a_reference_result_returns_the_wrapper_like_kotlinc() {
    let sources = [("FunInterfaces.kt", FUN_INTERFACES)];
    let wrapper = common::ModuleClassPair::compile(&sources, "FunInterfacesKt$sam$Child$0");
    assert!(
        wrapper.krusty == wrapper.kotlinc,
        "the function-value wrapper differs from kotlinc"
    );
    let implements_boxed_result = |bytes: &[u8]| {
        krusty::jvm::classreader::parse_class(bytes)
            .expect("the lambda class parses")
            .methods
            .iter()
            .any(|method| method.name == "f" && method.descriptor == "()Ljava/lang/Integer;")
    };
    let lambda = common::ModuleClassPair::compile(&sources, "FunInterfacesKt$lambda$1");
    assert!(
        implements_boxed_result(&lambda.kotlinc),
        "kotlinc's lambda class"
    );
    assert!(
        implements_boxed_result(&lambda.krusty),
        "krusty's lambda class"
    );
}
