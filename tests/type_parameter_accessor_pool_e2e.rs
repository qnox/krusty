//! A declared accessor over a type-parameter-typed field interns its nullability annotation with
//! its header, before its body, as kotlinc visits it.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
interface Cargo\n\
\n\
open class Base<T> {\n\
    open var x: T? = null\n\
}\n\
\n\
class Held<T> {\n\
    var w: T? = null\n\
}\n\
\n\
open class Bound<T : Cargo> {\n\
    open var x: T? = null\n\
    var y: T? = null\n\
}\n\
";

#[test]
fn a_nullable_type_parameter_getter_interns_its_annotation_with_its_header() {
    for class in ["store/Base", "store/Held", "store/Bound"] {
        common::byte_diff_against_kotlinc_cp(
            "TypeParameterAccessorPool",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
