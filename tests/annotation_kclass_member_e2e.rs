//! A Kotlin annotation member declared `KClass` is a `java.lang.Class` on the JVM.
//!
//! `equals` rebuilds the `KClass`, so `Int::class` and `Integer::class` compare equal, while
//! `hashCode` hashes the stored `Class` and those two hashes differ.

use std::collections::BTreeMap;

fn member_abi(bytes: &[u8]) -> BTreeMap<String, (String, Option<String>)> {
    krusty::jvm::classreader::parse_class(bytes)
        .expect("parse annotation class")
        .methods
        .into_iter()
        .map(|method| (method.name, (method.descriptor, method.signature)))
        .collect()
}

fn assert_same_member_abi(stem: &str, source: &str, class_name: &str) {
    let pairs = common::compile_with_kotlinc(stem, source, &[], &[class_name]);
    let (reference, actual) = &pairs[0];
    assert_eq!(
        member_abi(actual),
        member_abi(reference),
        "{stem}: annotation member descriptors and signatures diverge from kotlinc"
    );
}

#[test]
fn kclass_annotation_members_match_kotlinc_abi() {
    assert_same_member_abi(
        "kclass_member_abi",
        "import kotlin.reflect.KClass\n\
         annotation class Mark(\n\
             val k: KClass<*>,\n\
             val ks: Array<KClass<*>>,\n\
             val out: KClass<out Number>,\n\
             val inn: KClass<in String>,\n\
             val exact: KClass<String>,\n\
             val primitive: KClass<Int>,\n\
             val nested: KClass<List<String>>,\n\
             val any: KClass<Any>,\n\
         )\n",
        "Mark",
    );
}

#[test]
fn kclass_annotation_impl_stores_java_class() {
    let classes = common::compile_in_process(
        "import kotlin.reflect.KClass\n\
         annotation class Mark(val k: KClass<*>, val ks: Array<KClass<*>>)\n\
         fun make() = Mark(String::class, arrayOf(Int::class))\n",
        "KClassStore",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )
    .expect("emit a KClass annotation instantiation");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name.contains("$annotationImpl$"))
        .expect("annotation implementation class");
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse impl");
    let field = |name: &str| {
        class
            .fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("missing field {name}"))
            .descriptor
            .clone()
    };
    assert_eq!(field("k"), "Ljava/lang/Class;");
    assert_eq!(field("ks"), "[Ljava/lang/Class;");
    let accessor = |name: &str| {
        class
            .methods
            .iter()
            .find(|method| method.name == name)
            .unwrap_or_else(|| panic!("missing accessor {name}"))
            .descriptor
            .clone()
    };
    assert_eq!(accessor("k"), "()Ljava/lang/Class;");
    assert_eq!(accessor("ks"), "()[Ljava/lang/Class;");
}

#[test]
fn kclass_annotation_equality_and_reads_match_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "import kotlin.reflect.KClass\n\
         annotation class Mark(val k: KClass<*>, val ks: Array<KClass<*>>)\n\
         enum class E { A }\n\
         annotation class EnumMark(val e: E, val k: KClass<*>)\n\
         fun box(): String {\n\
             val a = Mark(Int::class, arrayOf(String::class, Int::class))\n\
             val b = Mark(Integer::class, arrayOf(String::class, Int::class))\n\
             if (a != b) return \"not-equal\"\n\
             if (a.hashCode() == b.hashCode()) return \"hash-same\"\n\
             if (a.k != Int::class || a.k != Integer::class) return \"read-k\"\n\
             if (a.ks[0] != String::class) return \"read-ks\"\n\
             val reordered = Mark(Int::class, arrayOf(Int::class, String::class))\n\
             if (a == reordered) return \"order\"\n\
             val tagged = EnumMark(E.A, String::class)\n\
             if (tagged.e != E.A || tagged.k != String::class) return \"enum\"\n\
             if (!tagged.toString().contains(\"EnumMark(\")) return \"tostring\"\n\
             return \"OK\"\n\
         }\n",
        "kclass_annotation_equality",
    );
}

#[test]
fn kclass_annotation_hash_matches_kotlinc() {
    let source = "import kotlin.reflect.KClass\n\
         annotation class Mark(val k: KClass<*>, val ks: Array<KClass<*>>)\n\
         fun box(): String = Mark(Int::class, arrayOf(String::class)).hashCode().toString()\n";
    let reference = common::kotlinc_box_result(source);
    let actual = common::expect_box_run_with_stdlib(source, "kclass_annotation_hash");
    assert_eq!(actual, reference, "stored Class hash diverges from kotlinc");
    reference
        .parse::<i32>()
        .unwrap_or_else(|_| panic!("kotlinc hash is not an Int: {reference}"));
}
