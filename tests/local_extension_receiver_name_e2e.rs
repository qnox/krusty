//! A local extension function's receiver is named after the function's lifted name.
//!
//! kotlinc names an extension receiver `$this$<name>` from the function's IR name when it writes
//! the method. `LocalDeclarationsLowering` has renamed a local function to its lifted name by then
//! (`top$wrap`), and the `$` in it is mangled for a JVM local: `$this$top_u24wrap`.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun top(): String {\n\
                      \x20   fun String.wrap() = this\n\
                      \x20   return \"O\".wrap() + \"K\".wrap()\n\
                      }\n\
                      \n\
                      class Holder {\n\
                      \x20   fun member(): String {\n\
                      \x20       fun String.twice() = this + this\n\
                      \x20       return \"a\".twice()\n\
                      \x20   }\n\
                      }\n";

#[test]
fn local_extension_receiver_takes_the_lifted_name_like_kotlinc() {
    for class in ["store/LocalExtensionReceiverNameKt", "store/Holder"] {
        common::byte_diff_against_kotlinc_cp(
            "LocalExtensionReceiverName",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
