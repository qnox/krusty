//! `x!!` yields `x` or fails, so its type is the operand's with the nullability taken off.
//!
//! The lowering already knew that — it unboxes a nullable primitive there — but the TYPE table did
//! not say so, and a consumer of `x!!` was left holding a value it could not name. `c!!.toInt()` on
//! a `Char?` then reached the conversion with a reference where a machine value was required, and
//! declined as "a value of undetermined type". A `lateinit` read has the same shape: it yields its
//! operand and only the guard differs.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// `!!` on a nullable primitive, then a member of the value it yields.
#[test]
fn a_not_null_assertion_yields_a_value_its_consumer_can_name() {
    let source = "fun box(): String {\n\
         \x20   val c: Char? = '0'\n\
         \x20   if (c!!.code != 48) return \"fail char\"\n\
         \x20   val n: Int? = 7\n\
         \x20   if (n!! + 1 != 8) return \"fail int\"\n\
         \x20   val s: Short? = 3\n\
         \x20   if (s!! + 1 != 4) return \"fail short\"\n\
         \x20   val b: Byte? = 2\n\
         \x20   if (b!! + 1 != 3) return \"fail byte\"\n\
         \x20   val d: Double? = 1.5\n\
         \x20   if (d!! + 0.5 != 2.0) return \"fail double\"\n\
         \x20   // Through a safe call, which is the shape the corpus's `kt4098` is written in.\n\
         \x20   if (\"123456\"?.get(0)!!.code != 49) return \"fail safe call\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "NotNullAssertType");
    expect_native_box(source, "NotNullAssertType", "OK");
}
