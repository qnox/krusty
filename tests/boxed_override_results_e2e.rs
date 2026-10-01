//! Dependency, delegation and SAM coverage for primitive overrides whose JVM result stays boxed.
//! Local and same-module declarations are covered by `local_override_boxed_result_e2e.rs` and
//! `module_override_boxed_result_e2e.rs`.

use super::common;

const LIBRARY: &str = "package lib
interface Source<T> { fun take(): T }
open class Counter : Source<Int> { override fun take(): Int = 3 }
";

const DEPENDENT: &str = "import lib.*
class Doubled : Counter() { override fun take(): Int = super.take() * 2 }
fun read(counter: Counter, doubled: Doubled): Int = counter.take() + doubled.take()
fun box(): String = if (read(Counter(), Doubled()) == 9) \"OK\" else \"fail\"
";

/// A dependency compiled by kotlinc already returns its boxed result: an override of it keeps the
/// same descriptor, needs no bridge, and a call unboxes the wrapper.
#[test]
fn an_override_of_a_boxed_dependency_result_stays_boxed() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIBRARY, DEPENDENT)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
    let classes =
        common::classes_against_kotlinc_lib("Dependent", &[("Lib.kt", LIBRARY)], DEPENDENT)
            .expect("reference kotlinc is provisioned");
    let boxed_take_nullability = |bytes: &[u8]| {
        krusty::jvm::classreader::parse_class(bytes)
            .expect("Doubled.class parses")
            .methods
            .into_iter()
            .filter(|method| method.name == "take" && method.descriptor == "()Ljava/lang/Integer;")
            .map(|method| method.return_nullability)
            .collect::<Vec<_>>()
    };
    let expected = vec![Some(krusty::jvm::classreader::JavaNullability::NotNull)];
    assert_eq!(
        boxed_take_nullability(&classes.reference["Doubled"]),
        expected,
        "kotlinc boxed override declaration"
    );
    assert_eq!(
        boxed_take_nullability(&classes.krusty["Doubled"]),
        expected,
        "krusty boxed override declaration"
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const FORWARDER: &str = "import lib.*
interface Box<T> { fun get(): T }
class Fixed : Box<Int> { override fun get(): Int = 4 }
class Forward(box: Box<Int>) : Box<Int> by box
class Counting(source: Source<Int>) : Source<Int> by source
fun box(): String {
    val box: Box<Int> = Forward(Fixed())
    val source: Source<Int> = Counting(Counter())
    return if (box.get() + source.take() == 7) \"OK\" else \"fail\"
}
";

/// A delegation forwarder of a generic member overrides a non-primitive result too. It returns the
/// delegate's result as the wrapper it already is: kotlinc casts it, and never unboxes and reboxes.
/// Only the forwarders' code and lines are compared: they still lack kotlinc's local variables.
#[test]
fn a_delegation_forwarder_returns_the_delegates_boxed_result() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIBRARY, FORWARDER)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
    let classes =
        common::classes_against_kotlinc_lib("Forwarder", &[("Lib.kt", LIBRARY)], FORWARDER)
            .expect("reference kotlinc is provisioned");
    for (class, declaration) in [
        ("Forward", "public java.lang.Integer get();"),
        ("Counting", "public java.lang.Integer take();"),
    ] {
        let without_locals = |listing: String| {
            listing
                .lines()
                .take_while(|line| *line != "LocalVariableTable:")
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (reference, krusty) = classes.method_listing(class, declaration);
        assert_eq!(
            without_locals(krusty),
            without_locals(reference),
            "{class}: kotlinc's forwarder"
        );
    }
}

const FUN_INTERFACES: &str = "package lib
interface Base { fun f(): Any }
fun interface Child : Base { override fun f(): Int }
interface Generic<T> { fun g(): T }
fun interface Inherited : Generic<Int>
fun interface Declared : Generic<Int> { override fun g(): Int }
";

const LAMBDAS: &str = "import lib.*
fun box(): String {
    val sum = Child { 10 }.f() + Inherited { 1 }.g() + Declared { 2 }.g()
    return if (sum == 13) \"OK\" else \"fail\"
}
";

/// A lambda converted to a functional interface implements the method's boxed result: the
/// interface declares `f()Integer`, and a closure implementing `f()I` would implement nothing.
/// An inherited generic method keeps its erased result.
#[test]
fn a_fun_interface_lambda_implements_the_boxed_result() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(FUN_INTERFACES, LAMBDAS)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
