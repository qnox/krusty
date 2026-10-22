//! A Java source field initializer is skipped up to the comma or semicolon that ends its
//! declarator. A comma inside type arguments does not end it: explicit method type arguments
//! (`Collections.<K, V>emptyMap()`, the shape protobuf-generated map-entry holders use), the type
//! after `new`, and the type after `instanceof` are type positions. A `<` anywhere else compares,
//! so a following comma still starts the next declarator. A type-use annotation inside type
//! arguments is skipped whole, so a comparison in its arguments neither closes nor opens them.

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
             fun after(): Int = Holder.AFTER\n\
             fun greater(): List<String> = Holder.GREATER\n\
             fun lesser(): List<String> = Holder.LESSER\n\
             fun tail(): Int = Holder.TAIL\n",
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
             \x20   public static final java.util.List<String> GREATER = new java.util.ArrayList<@A(1 > 0) String>(),\n\
             \x20       LESSER = new java.util.ArrayList<@A(1 < 2) String>();\n\
             \x20   public static final int TAIL = 9;\n\
             }\n",
        ),
        (
            "p/A.java",
            "package p;\n\
             \n\
             import java.lang.annotation.ElementType;\n\
             import java.lang.annotation.Target;\n\
             \n\
             @Target(ElementType.TYPE_USE)\n\
             public @interface A {\n\
             \x20   boolean value();\n\
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
