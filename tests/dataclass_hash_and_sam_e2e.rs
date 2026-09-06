//! Two backend paths the corpus underexercises: a `data class` whose fields are `Double`/`Long`/
//! `Float` (its synthesized `hashCode` calls the static `Double.hashCode`/`Long.hashCode`/
//! `Float.hashCode` helpers) and a lambda converted to a `void`-returning SAM interface (`Runnable`),
//! whose bridge body loads `Unit.INSTANCE` after the call.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_ok_with_stdlib(body, stem);
}

#[test]
fn data_class_float_field_hashcode() {
    run_ok(
        "DataHash",
        "data class Nums(val d: Double, val l: Long, val f: Float)\n\
         fun box(): String {\n\
         val a = Nums(1.5, 7L, 2.5f)\n\
         val b = Nums(1.5, 7L, 2.5f)\n\
         if (a != b) return \"eq\"\n\
         if (a.hashCode() != b.hashCode()) return \"hc\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn data_class_unsigned_fields_use_value_class_hash_and_string_semantics() {
    run_ok(
        "DataUnsignedHashString",
        "data class Unsigned(val uint: UInt, val ulong: ULong)\n\
         fun box(): String {\n\
         val value = Unsigned(UInt.MAX_VALUE, ULong.MAX_VALUE)\n\
         val expected = \"Unsigned(uint=4294967295, ulong=18446744073709551615)\"\n\
         if (value.toString() != expected) return value.toString()\n\
         if (value.hashCode() != -31) return \"hash=${value.hashCode()}\"\n\
         if (value != Unsigned(UInt.MAX_VALUE, ULong.MAX_VALUE)) return \"equals\"\n\
         return \"OK\"\n\
         }\n",
    );
}

#[test]
fn data_class_keeps_declared_equals() {
    run_ok(
        "DataEquals",
        "data class A(val x: Int) {\n\
         override fun equals(other: Any?): Boolean = false\n\
         }\n\
         fun box(): String { val a = A(0); return if (a == a) \"fail\" else \"OK\" }\n",
    );
}

#[test]
fn data_class_copy_does_not_treat_delegate_storage_as_a_capture() {
    run_ok(
        "DataDelegationCopy",
        "interface A {\n\
         fun foo(): String\n\
         val bar: String\n\
         }\n\
         class B : A {\n\
         override fun foo() = \"O\"\n\
         override val bar get() = \"K\"\n\
         }\n\
         data class C(val a: A) : A by a\n\
         fun box(): String {\n\
         val c = C(B())\n\
         val copied = c.copy()\n\
         return copied.foo() + copied.bar\n\
         }\n",
    );
}

#[test]
fn lambda_to_void_sam() {
    run_ok(
        "SamVoid",
        "fun box(): String {\n\
         var x = 0\n\
         val r = Runnable { x = 5 }\n\
         r.run()\n\
         return if (x == 5) \"OK\" else \"x=$x\"\n\
         }\n",
    );
}
