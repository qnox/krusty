//! `UInt`/`ULong` operations in the shape of kotlinc's JVM backend.
//!
//!   * `compareTo`, `div`, `rem` and `toString()` of the same unsigned type are the JDK's unsigned
//!     operations on the carrier (`Integer.compareUnsigned`, `divideUnsigned`, `remainderUnsigned`,
//!     `toUnsignedString`), not calls to or inlined bodies of the stdlib declarations;
//!   * a string template renders an unsigned operand through its class's `toString-impl` and
//!     appends it as an `Object`, like any other value class.
use super::common;

#[test]
fn unsigned_members_are_jdk_unsigned_operations() {
    let name = "UnsignedMembers";
    let src = "fun less(a: UInt, b: UInt) = a < b\n\
               fun compare(a: ULong, b: ULong) = a.compareTo(b)\n\
               fun div(a: UInt, b: UInt) = a / b\n\
               fun rem(a: ULong, b: ULong) = a % b\n\
               fun text(a: UInt, b: ULong) = a.toString() + b.toString()\n";
    let class = format!("{name}Kt");
    common::byte_diff_against_kotlinc_cp(name, src, &class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
}

#[test]
fn unsigned_template_operands_render_through_to_string_impl() {
    let name = "UnsignedTemplates";
    let src = "fun two(a: ULong, b: UInt) = \"x$a y$b\"\n\
               fun narrow(a: UByte, b: UShort) = \"x$a$b\"\n";
    let class = format!("{name}Kt");
    common::byte_diff_against_kotlinc_cp(name, src, &class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
}

#[test]
fn unsigned_operations_still_compute_unsigned_results() {
    let src = "fun box(): String {\n\
               \x20   val big = UInt.MAX_VALUE - 1u\n\
               \x20   val long = ULong.MAX_VALUE\n\
               \x20   val order = \"${big < 3u} ${long.compareTo(1uL)}\"\n\
               \x20   val arithmetic = \"${big / 3u} ${big % 7u} ${long / 10uL} ${long % 10uL}\"\n\
               \x20   return \"$order $arithmetic ${big.toString()} ${long.toString()} x${255.toUByte()}\"\n\
               }\n";
    let actual =
        common::compile_and_run_box(src, "unsigned_operations", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(
        actual,
        "false 1 1431655764 2 1844674407370955161 5 4294967294 18446744073709551615 x255"
    );
}
