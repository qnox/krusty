//! A primary constructor that takes a value class is hidden behind a `DefaultConstructorMarker`
//! accessor, but when every parameter has a default kotlinc still adds the public no-arg
//! convenience constructor beside it. krusty took the hidden constructor's private access for it
//! and left it out.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    let comparison = common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()])
        .expect("reference class comparison must be available");
    if let Err(e) = comparison {
        panic!("{e}");
    }
}

const SOURCE: &str = r#"
@JvmInline
value class Time(val value: String)
class Condition(val time: Time = Time("now"), val message: String? = null)
class Nullable(val time: Time? = null)
open class Base protected constructor(val time: Time = Time("now"))
"#;

#[test]
fn a_defaulted_value_class_parameter_keeps_the_no_arg_constructor() {
    for class in ["Condition", "Nullable", "Base"] {
        byte_identical("value_class_parameter_no_arg_constructor", SOURCE, class);
    }
}
