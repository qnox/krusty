//! A lambda invoked inside nested inline calls is spliced, including one copied through each
//! level as an unnamed temporary.

use super::common;

#[test]
fn a_lambda_invoked_inside_nested_uses_is_spliced() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "NestedUseLambda",
        "import java.io.OutputStream\n\
         import java.util.zip.ZipOutputStream\n\
         \n\
         inline fun zip(out: OutputStream, shuffle: (MutableList<String>) -> Unit) {\n\
             out.use { outputStream ->\n\
                 ZipOutputStream(outputStream).use {\n\
                     shuffle(mutableListOf())\n\
                 }\n\
             }\n\
         }\n\
         fun go(out: OutputStream) = zip(out) {}\n",
        &["NestedUseLambdaKt"],
    );
}

#[test]
fn an_omitted_default_lambda_inside_nested_uses_is_spliced() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "NestedUseDefault",
        "import java.io.OutputStream\n\
         import java.util.zip.ZipOutputStream\n\
         \n\
         inline fun zip(out: OutputStream, shuffle: (MutableList<String>) -> Unit = {}) {\n\
             out.use { outputStream ->\n\
                 ZipOutputStream(outputStream).use {\n\
                     shuffle(mutableListOf())\n\
                 }\n\
             }\n\
         }\n\
         fun go(out: OutputStream) = zip(out)\n",
        &["NestedUseDefaultKt", "NestedUseDefaultKt$zip$1"],
    );
}
