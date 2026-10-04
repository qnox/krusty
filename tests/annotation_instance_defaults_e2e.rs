//! An annotation instantiation that omits elements takes the annotation declaration's defaults.
//!
//! kotlinc substitutes each omitted default at the construction. A default declared in another
//! source file is lowered there from its checked expression; a dependency annotation's defaults
//! are its classfile `AnnotationDefault` values. A construction inside a default runs where the
//! constructions omitting it are, so the declaring class owns no implementation class for it.

use super::common;

const DECLARATIONS: &str = "package sample\n\
import kotlin.reflect.KClass\n\
enum class Shade { LIGHT, DARK }\n\
annotation class Leaf(val tag: String = \"leaf\")\n\
annotation class Tree(\n\
    val count: Int = 42,\n\
    val small: Byte = 7,\n\
    val leaf: Leaf = Leaf(),\n\
    val kind: KClass<*> = Leaf::class,\n\
    val referenceArrayKind: KClass<*> = Array<String>::class,\n\
    val primitiveArrayKind: KClass<*> = IntArray::class,\n\
    val kinds: Array<KClass<*>> = [Shade::class, Leaf::class],\n\
    val shade: Shade = Shade.DARK,\n\
    val names: Array<String> = [\"a\", \"b\"],\n\
    val numbers: IntArray = [1, 2],\n\
)\n";

const USE: &str = "package sample\n\
fun box(): String {\n\
    val tree = Tree()\n\
    val seven: Byte = 7\n\
    if (tree.count != 42 || tree.small != seven) return \"numbers\"\n\
    if (tree.leaf != Leaf() || tree.leaf.tag != \"leaf\") return \"nested\"\n\
    if (tree.kind != Leaf::class) return \"class\"\n\
    if (tree.referenceArrayKind != Array<String>::class) return \"reference array class\"\n\
    if (tree.primitiveArrayKind != IntArray::class) return \"primitive array class\"\n\
    if (tree.kinds.size != 2 || tree.kinds[0] != Shade::class || tree.kinds[1] != Leaf::class) return \"classes\"\n\
    if (tree.shade != Shade.DARK) return \"enum\"\n\
    if (tree.names.size != 2 || tree.names[0] != \"a\" || tree.names[1] != \"b\") return \"names\"\n\
    if (tree.numbers.size != 2 || tree.numbers[0] != 1 || tree.numbers[1] != 2) return \"ints\"\n\
    val partial = Tree(count = 1, shade = Shade.LIGHT)\n\
    if (partial.count != 1 || partial.shade != Shade.LIGHT || partial.leaf != tree.leaf) return \"partial\"\n\
    return \"OK\"\n\
}\n";

fn class_names(classes: &std::collections::BTreeMap<String, Vec<u8>>) -> Vec<&str> {
    classes.keys().map(String::as_str).collect()
}

fn element_defaults(
    classes: &std::collections::BTreeMap<String, Vec<u8>>,
    class: &str,
) -> Vec<(Box<str>, krusty::libraries::AnnotationElementDefault)> {
    krusty::jvm::classreader::parse_class(&classes[class])
        .expect("parse annotation class")
        .annotation_element_defaults
}

#[test]
fn defaults_declared_in_another_file_run_like_kotlinc() {
    let sources = [("Declarations.kt", DECLARATIONS), ("Use.kt", USE)];
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "sample.UseKt"),
        "OK"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}

#[test]
fn defaults_declared_in_another_file_write_kotlinc_classes_and_element_defaults() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declarations.kt", DECLARATIONS),
        ("Use.kt", USE),
    ]);
    assert_eq!(
        class_names(&classes.krusty),
        class_names(&classes.reference)
    );
    for annotation in ["sample/Leaf", "sample/Tree"] {
        assert_eq!(
            element_defaults(&classes.krusty, annotation),
            element_defaults(&classes.reference, annotation),
            "{annotation}: AnnotationDefault values diverge from kotlinc"
        );
    }
}

#[test]
fn a_construction_inside_a_default_belongs_to_the_consuming_scope() {
    let source = "annotation class Leaf()\n\
        annotation class Branch(val leaf: Leaf = Leaf())\n\
        annotation class Unused(val leaf: Leaf = Leaf())\n\
        class Holder { fun make(): Branch = Branch() }\n\
        fun box(): String = if (Holder().make().leaf == Leaf()) \"OK\" else \"fail\"\n";
    let classes = common::classes_against_kotlinc_module(&[("Owner.kt", source)]);
    assert_eq!(
        class_names(&classes.krusty),
        [
            "Branch",
            "Holder",
            "Holder$annotationImpl$Branch$0",
            "Holder$annotationImpl$Leaf$0",
            "Leaf",
            "OwnerKt",
            "Unused",
        ]
    );
    assert_eq!(
        class_names(&classes.krusty),
        class_names(&classes.reference)
    );
    assert_eq!(common::expect_box_run_with_stdlib(source, "Owner"), "OK");
}

#[test]
fn dependency_defaults_come_from_annotation_default() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(DECLARATIONS, USE).as_deref(),
        Some("OK")
    );
    assert_eq!(
        common::expect_box_run_against("annotation_dependency_defaults", DECLARATIONS, USE)
            .as_deref(),
        Some("OK")
    );
    let classes = common::classes_against_kotlinc_lib("Use", &[("Lib.kt", DECLARATIONS)], USE)
        .expect("reference toolchain is provisioned");
    assert_eq!(
        class_names(&classes.krusty),
        class_names(&classes.reference)
    );
}
