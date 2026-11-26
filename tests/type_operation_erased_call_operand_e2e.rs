//! A type operation over a generic call's result applies its null check and cast to the erased
//! `Object` the call leaves, as in kotlinc, rather than to the result narrowed first.

use super::common;

const SOURCE: &str = "data class Ref<D>(val t: D)\n\
                      fun <E> List<Ref<*>>.element(i: Int): Ref<E> = this[i] as Ref<E>\n\
                      fun <E> List<Ref<*>>.checked(i: Int): Ref<E>? = this[i] as? Ref<E>\n\
                      fun List<Ref<*>>.isRef(i: Int): Boolean = this[i] is Ref<*>\n";

#[test]
fn a_cast_over_a_generic_call_result_checks_the_erased_object() {
    common::byte_diff_against_kotlinc_cp(
        "TypeOperationErasedCallOperand",
        SOURCE,
        "TypeOperationErasedCallOperandKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("differs from kotlinc:\n{diff}"));
}
