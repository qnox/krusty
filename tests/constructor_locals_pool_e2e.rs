//! A primary constructor's `LocalVariableTable` strings join the constant pool before the class
//! entries of its frames, as kotlinc writes the table before it computes the frames.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
var log = \"\"\n\
\n\
open class Base(val s: String)\n\
\n\
class Branchy(i: Int) : Base(\"O\" + if (i == 23) {\n\
    log += \"logged\"\n\
    \"K\"\n\
} else {\n\
    \"fail\"\n\
})\n\
\n\
class Chosen(p: String, q: String) {\n\
    val picked: String = if (p == \"x\") p else q\n\
}\n\
";

#[test]
fn constructor_locals_precede_frame_classes_in_the_pool() {
    for class in ["store/Branchy", "store/Chosen"] {
        common::byte_diff_against_kotlinc_cp(
            "ConstructorLocalsPool",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
