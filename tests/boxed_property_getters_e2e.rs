//! A primitive property overriding one whose type is not primitive returns the wrapper from its
//! getter, as a function's result does (see `boxed_override_results_e2e.rs`): `override val level:
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

/// Every class but the interfaces, as kotlinc writes it. An interface still records a field
/// descriptor kotlinc leaves out of its abstract property's metadata, which is not this change's.
#[test]
fn a_boxed_property_getter_matches_kotlinc_across_files() {
    let sources = [
        ("Declarations.kt", DECLARATIONS),
        ("Overrides.kt", OVERRIDES),
    ];
    for class in [
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
