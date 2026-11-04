//! A nested call argument's open result variables belong to the enclosing call; the formals of the
//! declarations around the call stay fixed. The enum-entry program is a shared box regression; this
//! test keeps its JVM runtime oracle.

use super::common;

#[test]
fn an_enum_entry_member_keeps_its_own_formal_fixed_in_nested_calls() {
    // `twice` has no transitional symbol-table record; its `T` is known only to the header. The
    // inner `same(value)` result is `T`, a fixed type at the outer call, so `twice("O")` is `String`.
    const SRC: &str = include_str!("box_regressions/enumEntryMemberNestedResult.kt");
    common::expect_box_ok_with_stdlib(SRC, "EnumEntryMemberNestedResult");
}
