//! A generic result substituted with a primitive (`Holder<Int>.get()`) arrives in an erased
//! `Object` slot already boxed. kotlinc coerces that slot straight to a reference consumer (`Any`, a
//! type parameter, `Int?`, a return of `Any`), so the value is never unboxed and boxed again; only
//! a primitive consumer unboxes it.
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
