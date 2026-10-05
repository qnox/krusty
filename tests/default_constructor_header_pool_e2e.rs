//! A constructor's `$default` overload interns its own header before the call it delegates to.
//!
//! ASM's `visitMethod` interns a method's name and descriptor when the method is begun, so the
//! synthetic `<init>(…, int, DefaultConstructorMarker)` descriptor precedes the `Methodref` its body
//! names for the real constructor. A primary and a secondary constructor with defaults both show it.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      open class Shelf(val label: String, val count: Int = 3) {\n\
                      \x20   constructor(x: Int, y: Int = 99) : this(\"$x$y\")\n\
                      }\n\
                      \n\
                      class Crate(x: Int) : Shelf(x)\n";

#[test]
fn default_constructor_header_precedes_its_body_constants_like_kotlinc() {
    for class in ["store/Shelf", "store/Crate"] {
        common::byte_diff_against_kotlinc_cp(
            "DefaultConstructorHeaderPool",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
