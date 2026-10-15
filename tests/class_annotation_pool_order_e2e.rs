//! kotlinc's class writer visits every method, then the field table (field names, `ConstantValue`
//! constants, and field annotations), and only then the class's own annotations before
//! `@Metadata`. A class annotation therefore interns after a companion's hoisted `Companion` and
//! `const` fields, an object's `const` fields, and a `@JvmField`'s annotations.
use super::common;

const SOURCE: &str = "package pool\n\
    \n\
    @Deprecated(\"d\", level = DeprecationLevel.WARNING)\n\
    @Suppress(\"unused\")\n\
    interface Hashing {\n\
    \x20   companion object {\n\
    \x20       const val SIZE: Int = 16\n\
    \x20       const val LOAD: Float = .75f\n\
    \x20   }\n\
    }\n\
    \n\
    @Deprecated(\"d\")\n\
    class Holder {\n\
    \x20   companion object {\n\
    \x20       const val A: Int = 16\n\
    \x20   }\n\
    }\n\
    \n\
    @Deprecated(\"d\")\n\
    object Constants {\n\
    \x20   const val A: Int = 16\n\
    }\n\
    \n\
    @Deprecated(\"d\")\n\
    class Exposed {\n\
    \x20   @JvmField val y: String = \"a\"\n\
    \x20   val z: Int = 1\n\
    }\n";

#[test]
fn class_annotations_intern_after_the_field_table() {
    for (name, class) in [
        ("ClassAnnotationPoolOrderHashing", "pool/Hashing"),
        ("ClassAnnotationPoolOrderHolder", "pool/Holder"),
        ("ClassAnnotationPoolOrderConstants", "pool/Constants"),
        ("ClassAnnotationPoolOrderExposed", "pool/Exposed"),
    ] {
        common::byte_diff_against_kotlinc_cp(name, SOURCE, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
