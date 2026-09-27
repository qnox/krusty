//! kotlinc's `Equals` intrinsic compares by reference (`if_acmpeq`/`if_acmpne`) when either
//! operand's type is an enum class, nullable or not, from source or from a library, instead of
//! calling `Intrinsics.areEqual`; the comparison's line is marked before the jump as for identity.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    enum class Season { SPRING, SUMMER }\n\
    \n\
    fun sink(x: Int) {}\n\
    fun pick(s: Season?): Season? = s\n\
    \n\
    fun eq(a: Season, b: Season) {\n\
    \x20   if (a == b) sink(1)\n\
    }\n\
    fun ne(a: Season) {\n\
    \x20   if (a != Season.SPRING) sink(2)\n\
    }\n\
    fun value(a: Season): Boolean = a == Season.SUMMER\n\
    fun valueNe(a: Season?): Boolean = a != Season.SUMMER\n\
    fun nullable(a: Season?) {\n\
    \x20   if (a ==\n\
    \x20       pick(a)) sink(3)\n\
    }\n\
    fun anyLeft(a: Any, b: Season): Boolean = a == b\n\
    fun anyRight(a: Season, b: Any?): Boolean = a != b\n\
    fun asReturn(a: Season): Boolean {\n\
    \x20   return Season.SPRING == a\n\
    }\n\
    fun library(a: DeprecationLevel): Boolean = a == DeprecationLevel.ERROR\n\
    fun nullCheck(a: Season?): Boolean = a == null\n";

#[test]
fn enum_equality_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "EnumEquality",
        SOURCE,
        "store/EnumEqualityKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("store/EnumEqualityKt byte-identical to kotlinc");
}
