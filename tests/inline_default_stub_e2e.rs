//! An `inline` function's `$default` stub contains the function body. A default lambda is a
//! class, not an `invokedynamic` bootstrap.

use super::common;

#[test]
fn an_inline_default_lambda_is_a_singleton_class() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultLambda",
        "inline fun f(block: () -> Unit = {}) { block() }\n",
        &["InlineDefaultLambdaKt", "InlineDefaultLambdaKt$f$1"],
    );
}

#[test]
fn an_inline_default_lambda_captures_its_parameter() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultCapture",
        "inline fun f(x: Int, block: () -> Int = { x }): Int = block()\n",
        &["InlineDefaultCaptureKt", "InlineDefaultCaptureKt$f$1"],
    );
}

#[test]
fn a_same_file_caller_still_materializes_the_default_lambda() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "InlineDefaultCaller",
        "import java.io.OutputStream\n\
         import java.util.zip.ZipOutputStream\n\
         \n\
         internal inline fun zip(out: OutputStream, shuffle: (MutableList<String>) -> Unit = {}) {\n\
         \x20   out.use { outputStream ->\n\
         \x20       ZipOutputStream(outputStream).use { zipOutputStream ->\n\
         \x20           val paths = mutableListOf<String>()\n\
         \x20           shuffle(paths)\n\
         \x20           paths.forEach { path ->\n\
         \x20               zipOutputStream.write(path.length)\n\
         \x20           }\n\
         \x20       }\n\
         \x20   }\n\
         }\n\
         fun go(out: OutputStream) = zip(out)\n",
        // The caller's `use` expansion is a separate difference. This pins the default
        // lambda class, which a same-file caller copies into its body.
        &["InlineDefaultCallerKt$zip$1"],
    );
}

#[test]
fn a_private_inline_default_lambda_stays_out_of_the_public_abi() {
    common::assert_classes_identical_to_kotlinc(
        "InlineDefaultPrivate",
        "private inline fun f(block: () -> Unit = {}) { block() }\n",
        &["InlineDefaultPrivateKt", "InlineDefaultPrivateKt$f$1"],
    );
}
