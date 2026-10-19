//! A generic result substituted with a primitive (`Holder<Int>.get()`) arrives in an erased
//! `Object` slot already boxed. kotlinc coerces that slot straight to a reference consumer (`Any`, a
//! type parameter, `Int?`, a return of `Any`), so the value is never unboxed and boxed again; only
//! a primitive consumer unboxes it.
//!
//! The rule does not depend on who declares the consumer. A dependency function, a dependency
//! constructor, or a member reached through a selected descriptor (`listOf(T)`, `Pair(A, B)`,
//! `KProperty0<Int>.get()` passed to `listOf`) takes the erased result unchanged too.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      class Holder<T>(val value: T) {\n\
                      \x20   fun get(): T = value\n\
                      }\n\
                      \n\
                      fun <T> share(a: T, b: T) {}\n\
                      \n\
                      fun take(a: Any) {}\n\
                      \n\
                      fun takeNullable(a: Int?) {}\n\
                      \n\
                      fun takeInt(a: Int) {}\n\
                      \n\
                      fun shared(h: Holder<Int>) { share(42, h.get()) }\n\
                      \n\
                      fun taken(h: Holder<Int>) { take(h.get()) }\n\
                      \n\
                      fun nullable(h: Holder<Int>) { takeNullable(h.get()) }\n\
                      \n\
                      fun primitive(h: Holder<Int>) { takeInt(h.get()) }\n\
                      \n\
                      fun returned(h: Holder<Long>): Any = h.get()\n\
                      \n\
                      fun property(h: Holder<Boolean>) { share(true, h.value) }\n";

#[test]
fn erased_scalar_results_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "ErasedScalarResult",
        SOURCE,
        "store/ErasedScalarResultKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/ErasedScalarResultKt differs from kotlinc: {diff}"));
}

const DEPENDENCY_SOURCE: &str = "package store\n\
                                 \n\
                                 val answer = 42\n\
                                 \n\
                                 class Box<T>(val value: T)\n\
                                 \n\
                                 fun <T> keep(value: T): T = value\n\
                                 \n\
                                 fun propertyValue(): List<Int> {\n\
                                 \x20   val p = ::answer\n\
                                 \x20   return listOf(p.get())\n\
                                 }\n\
                                 \n\
                                 fun element(l: List<Int>): List<Int> = listOf(l[0])\n\
                                 \n\
                                 fun kept(l: List<Char>): Any = keep(l[0])\n\
                                 \n\
                                 fun paired(l: List<Int>): Pair<Int, Long> = Pair(l[0], 2L)\n\
                                 \n\
                                 fun boxed(l: List<Long>): Box<Long> = Box(keep(l[0]))\n\
                                 \n\
                                 fun put(m: MutableMap<String, Int>, l: List<Int>) {\n\
                                 \x20   m.put(\"k\", l[0])\n\
                                 }\n\
                                 \n\
                                 fun printed(m: Map<String, Boolean>) {\n\
                                 \x20   println(m.getValue(\"k\"))\n\
                                 }\n";

#[test]
fn erased_scalar_results_passed_to_dependency_reference_slots_are_byte_identical_to_kotlinc() {
    for class in [
        "store/ErasedScalarDependencyArgumentKt",
        "store/ErasedScalarDependencyArgumentKt$propertyValue$p$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "ErasedScalarDependencyArgument",
            DEPENDENCY_SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
