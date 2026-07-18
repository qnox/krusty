//! Reading a NESTED singleton `object` through its enclosing SAME-FILE class name — `V.A` where
//! `object A` is declared inside `class/sealed class V`. The checker handled a CLASSPATH nested object
//! (`PrimitiveKind.STRING`) but had no case for a same-file one, so `V.A` fell through to checking the
//! receiver `V` as a value → "unresolved reference 'V'". Pervasive in sealed-result hierarchies
//! (mission-core `SlugValidation.TooShort` in a `when`).
use super::common;

#[test]
fn same_file_sealed_nested_object_read() {
    const SRC: &str = "sealed class V {\n\
        \x20 object A : V()\n\
        \x20 object B : V()\n\
        }\n\
        fun pick(n: Int): V = if (n < 3) V.A else V.B\n\
        fun box(): String =\n\
        \x20 if (pick(1) == V.A && pick(9) == V.B) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("same-file nested object read"),
        "OK"
    );
}
