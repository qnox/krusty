//! An explicit `Boolean.not()` call is the same negation as `!`: kotlinc's `not` intrinsic only
//! flips the jump of its operand (`if (b.not())` is `iload; ifne`) and materializes a value with a
//! branch over its operand. A callable reference keeps the selected `Boolean.not` declaration in its
//! adapter body, which the JVM target realizes at its own boundary; that realization must stay a
//! negation rather than become the equality `b == false`. Over a plain operand the two spellings
//! happen to fold to the same bytes, so `jvm::builtin_member_operations::tests` pins the recorded
//! negation itself; these differentials pin the emitted code against kotlinc.
use super::common;

const EXPLICIT_CALL: &str = "package store\n\
                             \n\
                             fun explicit(b: Boolean): Int {\n\
                             \x20   if (b.not()) return 1\n\
                             \x20   return 2\n\
                             }\n\
                             \n\
                             fun explicitLoop(b: Boolean, n: Int): Int {\n\
                             \x20   var i = n\n\
                             \x20   while (b.not() && i > 0) i--\n\
                             \x20   return i\n\
                             }\n\
                             \n\
                             fun explicitValue(b: Boolean): Boolean = b.not()\n";

#[test]
fn explicit_boolean_not_call_is_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "BooleanNotCall",
        EXPLICIT_CALL,
        "store/BooleanNotCallKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/BooleanNotCallKt differs from kotlinc: {diff}"));
}

const CALLABLE_REFERENCE: &str = "package store\n\
                                  \n\
                                  fun reference(): (Boolean) -> Boolean = Boolean::not\n";

#[test]
fn boolean_not_callable_reference_is_byte_identical_to_kotlinc() {
    for class in [
        "store/BooleanNotReferenceKt",
        "store/BooleanNotReferenceKt$reference$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "BooleanNotReference",
            CALLABLE_REFERENCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
