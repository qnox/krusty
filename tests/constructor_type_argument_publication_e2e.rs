//! A constructor call publishes its classifier's own type. Explicit type arguments substitute its
//! rigid type parameters as written, through a type alias or not; only an inferred argument takes
//! the flexibility of the Java usage it was inferred through.

use super::common;

const EXPLICIT: &str = r#"
val list = ArrayList<String>()
val set = HashSet<String>()
val map = LinkedHashMap<String, Int>()
val callbacks = ArrayList<(String, Int) -> Unit>()
val copied = ArrayList<String>(listOf("a"))
val sized = HashMap<String, Int>(4)
val direct = java.util.ArrayList<String>()

class Holder {
    val members = ArrayList<String>()
    var entries = LinkedHashMap<String, List<Int>>()
}
"#;

#[test]
fn explicit_constructor_type_arguments_publish_rigid_arguments() {
    common::assert_classes_identical_to_kotlinc(
        "ExplicitConstructorArguments",
        EXPLICIT,
        &["ExplicitConstructorArgumentsKt", "Holder"],
    );
}
