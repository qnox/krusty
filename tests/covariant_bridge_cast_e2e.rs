//! A bridge casts its delegated result to the bridge's return type whenever the two reference
//! types differ and the bridge's is not `Object`, as kotlinc's JVM coercion does: a covariant
//! override casts up to the supertype's declared class or interface.

use super::common;

const SOURCE: &str = r#"
open class Shape
class Square : Shape()

interface Named { fun name(): CharSequence }
class Plain : Named { override fun name(): String = "plain" }

interface Maker { fun make(): Shape }
class SquareMaker : Maker { override fun make(): Square = Square() }

abstract class Labelled { abstract val label: Comparable<String> }
class Fixed : Labelled() { override val label: String = "fixed" }
"#;

#[test]
fn a_covariant_override_bridge_casts_up_to_the_supertype_return_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "CovariantBridgeCast",
        SOURCE,
        &["Plain", "SquareMaker", "Fixed"],
    );
}
