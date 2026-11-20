//! A private top-level extension used from another class of its file is reached through the
//! facade's synthetic `access$<name>` bridge. kotlinc's bridge takes the extension receiver as an
//! ordinary parameter and names it `$receiver` in its local-variable table, as a private member
//! extension's bridge does; krusty named it after the target (`$this$<name>`).

use super::common;

#[test]
fn a_private_extension_accessor_names_its_receiver_receiver() {
    let source = "private fun Int.twice() = this * 2\n\
        private fun String.joinWith(other: String, sep: Char) = this + sep + other\n\
        class Holder {\n\
        \x20   fun f() = 3.twice()\n\
        \x20   fun g() = \"a\".joinWith(\"b\", '-')\n\
        \x20   private fun Int.thrice() = this * 3\n\
        \x20   inner class Inner { fun h() = 2.thrice() }\n\
        }\n";
    let classes =
        common::classes_against_kotlinc_module(&[("PrivateExtensionAccessor.kt", source)]);
    for class in ["PrivateExtensionAccessorKt", "Holder", "Holder$Inner"] {
        let (reference, krusty) = classes.class_listing(class);
        assert_eq!(
            krusty, reference,
            "{class}: kotlinc's code and debug tables"
        );
    }
}
