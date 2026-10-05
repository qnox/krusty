//! kotlinc hides the physical JDK members listed in `JvmBuiltInsSignatures.HIDDEN_METHOD_SIGNATURES`
//! from a mapped builtin whose scope otherwise joins its JVM class: `Double.isNaN()` resolves to the
//! stdlib's inline extension (`invokestatic java/lang/Double.isNaN(D)Z` on the unboxed value), never
//! to the boxed `java/lang/Double.isNaN()Z` instance method.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun doubleNaN(d: Double): Boolean = d.isNaN()\n\
                      \n\
                      fun floatNaN(f: Float): Boolean = f.isNaN()\n\
                      \n\
                      fun doubleInfinite(d: Double): Boolean = d.isInfinite()\n\
                      \n\
                      fun floatInfinite(f: Float): Boolean = f.isInfinite()\n\
                      \n\
                      fun branch(d: Double): Int {\n\
                      \x20   if (d.isNaN()) return 1\n\
                      \x20   return 2\n\
                      }\n";

#[test]
fn hidden_mapped_member_calls_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "HiddenMappedMember",
        SOURCE,
        "store/HiddenMappedMemberKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/HiddenMappedMemberKt differs from kotlinc: {diff}"));
}
