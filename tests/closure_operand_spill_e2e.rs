//! A closure whose body splices `use` does not spill the call that creates it.
//!
//! The `try` runs in the closure method. kotlinc leaves the receiver on the stack under the
//! captured arguments and the `invokedynamic`.

use super::common;

const SOURCE: &str = "\
fun consume(path: String, action: (String) -> Unit) {\n\
    action(path)\n\
}\n\
\n\
fun unpack(path: String, dest: String, reset: Boolean) {\n\
    consume(path) { value ->\n\
        if (reset) {\n\
            java.io.FileInputStream(value).use { input ->\n\
                println(dest + input.read())\n\
            }\n\
        }\n\
    }\n\
}\n\
";

#[test]
fn a_closure_keeps_its_receiver_on_the_stack() {
    let classes =
        common::ModuleClassPair::compile(&[("ClosureSpill.kt", SOURCE)], "ClosureSpillKt");
    let (reference, krusty) = classes.method_code("ClosureSpillKt", "unpack");
    assert_eq!(krusty, reference, "unpack");
}
