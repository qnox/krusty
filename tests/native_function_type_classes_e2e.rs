//! A CLASS that implements a function type, rather than a lambda.
//!
//! `invoke` is the one dependency member this target already gives a fixed slot: a function
//! value's body sits right after `kotlin.Any`'s three, the runtime names that number itself
//! (`KT_SLOT_INVOKE`), and every caller through a function type reads it. A class implementing
//! `Function0<T>` puts its `invoke` there for the same reason a lambda does — and until it did,
//! every such class was declined as an override of a dependency method.
//!
//! Only when every operand and the result are REFERENCES. A caller through the function type
//! passes and reads references, and an `invoke(x: Int): Int` carries machine integers — that one
//! needs a bridge and still declines.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// Called as a function value, and through the function type it wears.
#[test]
fn a_class_implementing_a_function_type_is_called_like_one() {
    let source = "class Greeter(val name: String) : Function0<String> {\n\
         \x20   override fun invoke(): String = \"hello \" + name\n\
         }\n\
         class Joiner : Function2<String, String, String> {\n\
         \x20   override fun invoke(a: String, b: String): String = a + b\n\
         }\n\
         fun call(f: () -> String): String = f()\n\
         fun box(): String {\n\
         \x20   val greeter = Greeter(\"world\")\n\
         \x20   if (greeter() != \"hello world\") return \"fail direct\"\n\
         \x20   if (call(greeter) != \"hello world\") return \"fail as a function value\"\n\
         \x20   val typed: () -> String = greeter\n\
         \x20   if (typed() != \"hello world\") return \"fail through the function type\"\n\
         \x20   if (Joiner()(\"O\", \"K\") != \"OK\") return \"fail arity two\"\n\
         \x20   // Its `kotlin.Any` members are still its own, which is what the fixed slot leaves\n\
         \x20   // room for: `invoke` sits AFTER the three, not on top of one.\n\
         \x20   if (greeter.toString() == \"\") return \"fail toString\"\n\
         \x20   if (greeter != greeter) return \"fail equals\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "FunctionTypeClass");
    expect_native_box(source, "FunctionTypeClass", "OK");
}

/// An `invoke` carrying MACHINE operands still declines: a caller through the function type passes
/// and reads references, so this one needs a bridge rather than the slot.
#[test]
fn an_invoke_carrying_machine_operands_still_declines() {
    expect_native_decline(
        "class Doubler : Function1<Int, Int> {\n\
         \x20   override fun invoke(x: Int): Int = x * 2\n\
         }\n\
         fun box(): String = if (Doubler()(21) == 42) \"OK\" else \"fail\"\n",
        "MachineInvoke",
        "cannot take the function slot",
    );
}
