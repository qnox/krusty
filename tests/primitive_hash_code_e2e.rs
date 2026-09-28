//! `hashCode()` on a primitive value hashes it through its wrapper's static `hashCode`
//! (`Integer.hashCode(I)I`) without boxing, as kotlinc's `HashCode` intrinsic does, in a direct
//! call and in an unbound callable reference's adapter alike.
use super::common;

const SOURCE: &str = "fun ofBoolean(v: Boolean) = v.hashCode()\n\
                      fun ofChar(v: Char) = v.hashCode()\n\
                      fun ofByte(v: Byte) = v.hashCode()\n\
                      fun ofShort(v: Short) = v.hashCode()\n\
                      fun ofInt(v: Int) = v.hashCode()\n\
                      fun ofLong(v: Long) = v.hashCode()\n\
                      fun ofFloat(v: Float) = v.hashCode()\n\
                      fun ofDouble(v: Double) = v.hashCode()\n\
                      fun ofSafeCall(v: Long?) = v?.hashCode()\n\
                      fun ofSmartCast(v: Boolean?) = if (v != null) v.hashCode() else 0\n\
                      fun intReference(): (Int) -> Int = Int::hashCode\n\
                      fun booleanReference(): (Boolean) -> Int = Boolean::hashCode\n";

#[test]
fn a_primitive_hash_code_calls_the_static_wrapper_method_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "PrimitiveHashCode",
        SOURCE,
        "PrimitiveHashCodeKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("PrimitiveHashCodeKt differs from kotlinc:\n{diff}"));
}

/// An unbound reference's adapter class calls the same static wrapper method: `Int::hashCode`
/// reaches `Any.hashCode` through the mapped wrapper, `Boolean::hashCode` through the builtins'
/// own declaration.
#[test]
fn a_primitive_hash_code_reference_adapter_calls_the_static_wrapper_method_like_kotlinc() {
    for class in [
        "PrimitiveHashCodeKt$intReference$1",
        "PrimitiveHashCodeKt$booleanReference$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "PrimitiveHashCode",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
    }
}

#[test]
fn a_primitive_hash_code_is_the_wrapper_hash() {
    let src = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   if (ofBoolean(true) != 1231 || ofBoolean(false) != 1237) return \"boolean\"\n\
         \x20   if (ofChar('A') != 65) return \"char\"\n\
         \x20   if (ofByte(-1) != -1 || ofShort(7) != 7 || ofInt(42) != 42) return \"int\"\n\
         \x20   if (ofLong(1L shl 32) != 1) return \"long\"\n\
         \x20   if (ofFloat(1.0f) != 1065353216) return \"float\"\n\
         \x20   if (ofDouble(1.0) != 1072693248) return \"double\"\n\
         \x20   if (ofSafeCall(null) != null || ofSafeCall(3L) != 3) return \"safe call\"\n\
         \x20   if (ofSmartCast(true) != 1231) return \"smart cast\"\n\
         \x20   if (intReference()(42) != 42) return \"int reference\"\n\
         \x20   if (booleanReference()(true) != 1231) return \"boolean reference\"\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    let actual =
        common::compile_and_run_box(&src, "primitive_hash_code", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
