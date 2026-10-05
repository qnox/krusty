//! kotlinc stores an inner class's enclosing instance at the start of a secondary constructor that
//! delegates to `super`, and that store carries the constructor's own line; the delegation's line
//! follows it. A constructor delegating to `this(…)` stores nothing and keeps its delegation line.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      open class Base(val n: Int)\n\
                      \n\
                      class Outer(val s: String) {\n\
                      \x20   inner class Implicit {\n\
                      \x20       val x: Int\n\
                      \x20       constructor(x: Int) {\n\
                      \x20           this.x = x\n\
                      \x20       }\n\
                      \x20   }\n\
                      \n\
                      \x20   inner class Explicit : Base {\n\
                      \x20       constructor(x: Int) :\n\
                      \x20           super(\n\
                      \x20               x\n\
                      \x20           )\n\
                      \x20       constructor(x: String) : this(x.length)\n\
                      \x20   }\n\
                      }\n";

#[test]
fn inner_secondary_constructors_are_byte_identical_to_kotlinc() {
    for class in ["store/Outer$Implicit", "store/Outer$Explicit"] {
        common::byte_diff_against_kotlinc_cp(
            "InnerSecondaryConstructorLine",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
