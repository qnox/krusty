//! kotlinc hands a function reference's carrier to its use site through an implicit cast, which
//! writes no instruction: the use site casts the carrier to the type it needs (`Function1` for a
//! function-typed parameter or return, `KFunction` for a local of the reference's own type) and
//! leaves it uncast where it is passed as `Any`.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun twice(n: Int): Int = n * 2\n\
                      \n\
                      fun take(f: (Int) -> Int): Int = f(1)\n\
                      fun takeAny(f: Any): Any = f\n\
                      \n\
                      class Holder(val x: Int) { fun plus(n: Int): Int = n + x }\n\
                      \n\
                      fun a(): Int = take(::twice)\n\
                      fun b(): Any = takeAny(::twice)\n\
                      fun c(): Int { val f = ::twice; return f(3) }\n\
                      fun d(): Int { val f: (Int) -> Int = ::twice; return f(3) }\n\
                      fun e(h: Holder): Int = take(h::plus)\n\
                      fun f(h: Holder): Any = takeAny(h::plus)\n\
                      fun g(): (Int) -> Int = ::twice\n\
                      fun h(h: Holder): Int { val f = h::plus; return f(2) }\n";

#[test]
fn function_reference_carriers_are_cast_at_their_use_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "ReferenceCarrierCast",
        SOURCE,
        "store/ReferenceCarrierCastKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/ReferenceCarrierCastKt differs from kotlinc: {diff}"));
}
