//! kotlinc stores a local class's captured values without a line, but an inner class's
//! enclosing-instance store keeps the class's line even when the inner class is itself local: the
//! constructor's first line entry sits on that `putfield this$0`, ahead of the delegation.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun plain(): Int {\n\
                      \x20   open class Outer {\n\
                      \x20       open inner class Inner {\n\
                      \x20           fun value() = 1\n\
                      \x20       }\n\
                      \x20   }\n\
                      \x20   return Outer().Inner().value()\n\
                      }\n\
                      \n\
                      fun anonymous(): Int {\n\
                      \x20   open class Outer {\n\
                      \x20       open inner class Inner {\n\
                      \x20           fun value() = 2\n\
                      \x20       }\n\
                      \x20   }\n\
                      \x20   val holder = object : Outer() {\n\
                      \x20       inner class Nested : Inner()\n\
                      \x20   }\n\
                      \x20   return holder.Nested().value()\n\
                      }\n";

#[test]
fn local_inner_class_constructors_are_byte_identical_to_kotlinc() {
    for class in [
        "store/LocalInnerEnclosingLineKt$plain$Outer$Inner",
        "store/LocalInnerEnclosingLineKt$anonymous$Outer$Inner",
        "store/LocalInnerEnclosingLineKt$anonymous$holder$1$Nested",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "LocalInnerEnclosingLine",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
