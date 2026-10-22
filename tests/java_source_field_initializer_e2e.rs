//! A Java source field initializer is skipped up to the comma or semicolon that ends its
//! declarator. A comma inside type arguments does not end it: explicit method type arguments
//! (`Collections.<K, V>emptyMap()`, the shape protobuf-generated map-entry holders use), the type
//! after `new`, and the type after `instanceof` are type positions. A `<` anywhere else compares,
//! so a following comma still starts the next declarator.

use super::common;

#[test]
fn a_comma_inside_initializer_type_arguments_stays_in_its_declarator_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        (
            "Reads.kt",
            "import p.Holder\n\
             \n\
             fun empty(): Map<String, Boolean> = Holder.EMPTY\n\
             fun created(): Map<String, Int> = Holder.CREATED\n\
             fun other(): Map<String, Int> = Holder.OTHER\n\
             fun checked(): Boolean = Holder.CHECKED\n\
             fun after(): Int = Holder.AFTER\n",
        ),
        (
            "p/Holder.java",
            "package p;\n\
             \n\
             import java.util.Collections;\n\
             import java.util.HashMap;\n\
             import java.util.Map;\n\
             \n\
             public final class Holder {\n\
             \x20   public static final Map<String, Boolean> EMPTY = Collections\n\
             \x20       .<String, Boolean>emptyMap();\n\
             \x20   public static final Map<String, Integer> CREATED = new HashMap<String, Integer>(),\n\
             \x20       OTHER = new java.util.TreeMap<String, Map<String, Integer>>().isEmpty() ? CREATED : null;\n\
             \x20   public static final boolean CHECKED = EMPTY instanceof Map<?, ?>, LESS = 1 < 2, MORE = 3 > 2;\n\
             \x20   public static final int AFTER = 7;\n\
             }\n",
        ),
    ]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
