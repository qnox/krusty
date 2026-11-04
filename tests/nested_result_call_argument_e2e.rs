//! A nested call argument's open result variables belong to the enclosing call; the formals of the
//! declarations around the call stay fixed. The Wasm KLIB lanes decline enum classes, so the
//! enum-entry member case runs here on the JVM.

use super::common;

#[test]
fn an_enum_entry_member_keeps_its_own_formal_fixed_in_nested_calls() {
    // `twice` has no transitional symbol-table record; its `T` is known only to the header. The
    // inner `same(value)` result is `T`, a fixed type at the outer call, so `twice("e")` is `String`.
    const SRC: &str = "fun <E> same(value: E): E = value\n\
enum class Kind {\n\
    A {\n\
        fun <T> twice(value: T) = same(same(value))\n\
        override fun run(): String = twice(\"O\") + twice(\"K\")\n\
    };\n\
    abstract fun run(): String\n\
}\n\
fun box(): String = Kind.A.run()\n";
    common::expect_box_ok_with_stdlib(SRC, "EnumEntryNestedResult");
}
