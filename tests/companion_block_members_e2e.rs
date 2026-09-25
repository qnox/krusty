//! `companion { … }` block members are static members of the classifier that declares the block:
//! kotlinc places their functions, accessors and backing fields on that class (initialized by the
//! class's `<clinit>`) and records them in the class's `@Metadata`, while a written
//! `companion fun C.f()` stays on the file facade.
use super::common;

const LANGUAGE: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n";

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(&format!("{LANGUAGE}{src}"), "Main")
}

fn javap_members(classes: &[(String, Vec<u8>)], class: &str) -> String {
    let dir = common::scratch_dir().expect("scratch directory");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == class)
        .unwrap_or_else(|| panic!("krusty did not emit {class}"));
    let path = dir.join(format!("{class}.class"));
    std::fs::write(&path, bytes).expect("write class file");
    common::javap(&["-p", &path.to_string_lossy()]).expect("javap")
}

#[test]
fn block_members_are_static_members_of_their_class() {
    const SRC: &str = "class A {\n\
        \x20   companion {\n\
        \x20       val v: String = \"O\"\n\
        \x20       fun f() = v + g()\n\
        \x20   }\n\
        }\n\
        companion fun A.g() = \"K\"\n\
        fun box() = A.f()\n";
    let classes = common::expect_classes_with_stdlib(&format!("{LANGUAGE}{SRC}"), "Main");
    let class = javap_members(&classes, "A");
    for member in [
        "private static final java.lang.String v;",
        "public static final java.lang.String getV();",
        "public static final java.lang.String f();",
        "static {};",
    ] {
        assert!(class.contains(member), "A lacks `{member}`:\n{class}");
    }
    let facade = javap_members(&classes, "MainKt");
    assert!(
        facade.contains("public static final java.lang.String g();")
            && !facade.contains(" f()")
            && !facade.contains("getV"),
        "only the written companion extension belongs on the facade:\n{facade}"
    );
    assert_eq!(run(SRC).expect("block members"), "OK");
}

#[test]
fn block_member_calls_block_member_and_companion_extension_unqualified() {
    const SRC: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun f(s: String) = g(s) + h()\n\
        \x20       fun h() = \"K\"\n\
        \x20   }\n\
        }\n\
        companion fun A.g(s: String) = s\n\
        fun box() = A.f(\"O\")\n";
    assert_eq!(run(SRC).expect("unqualified companion calls"), "OK");
}

#[test]
fn instance_member_calls_inherited_and_private_block_members() {
    const SRC: &str = "open class Base {\n\
        \x20   companion { fun base() = \"O\" }\n\
        }\n\
        class C : Base() {\n\
        \x20   companion {\n\
        \x20       private val k = \"K\"\n\
        \x20       private fun own() = k\n\
        \x20   }\n\
        \x20   fun ok() = base() + own()\n\
        }\n\
        fun box() = C().ok()\n";
    assert_eq!(run(SRC).expect("instance member calls"), "OK");
}

#[test]
fn block_property_initializes_with_its_class_not_the_file() {
    const SRC: &str = "var initialized = false\n\
        class Foo {\n\
        \x20   companion { val p = run { initialized = true; \"\" } }\n\
        }\n\
        companion val Foo.greeting: String = \"hi\"\n\
        fun box(): String {\n\
        \x20   if (Foo.greeting != \"hi\") return \"greeting\"\n\
        \x20   if (initialized) return \"a companion extension initialized its classifier\"\n\
        \x20   Foo.p\n\
        \x20   return if (initialized) \"OK\" else \"reading a block property did not\"\n\
        }\n";
    assert_eq!(run(SRC).expect("initialization order"), "OK");
}

#[test]
fn nested_class_block_members_belong_to_the_nested_class() {
    const SRC: &str = "class Outer {\n\
        \x20   class Nested {\n\
        \x20       companion { val v = \"OK\" }\n\
        \x20   }\n\
        }\n\
        fun box() = Outer.Nested.v\n";
    let classes = common::expect_classes_with_stdlib(&format!("{LANGUAGE}{SRC}"), "Main");
    let nested = javap_members(&classes, "Outer$Nested");
    assert!(
        nested.contains("public static final java.lang.String getV();"),
        "Outer$Nested lacks its block property accessor:\n{nested}"
    );
    assert_eq!(run(SRC).expect("nested block"), "OK");
}

#[test]
fn block_property_beside_companion_object_property_keeps_its_field_name() {
    const SRC: &str = "class E {\n\
        \x20   companion { val value = \"O\" }\n\
        \x20   companion object { val value = \"K\" }\n\
        }\n\
        fun box() = E.value + E.Companion.value\n";
    let classes = common::expect_classes_with_stdlib(&format!("{LANGUAGE}{SRC}"), "Main");
    let class = javap_members(&classes, "E");
    assert!(
        class.contains("private static final java.lang.String value;")
            && class.contains("private static final java.lang.String value$1;"),
        "the hoisted companion property takes the suffixed field:\n{class}"
    );
    assert_eq!(run(SRC).expect("field names"), "OK");
}

#[test]
fn property_reference_to_block_property_reads_its_class() {
    const SRC: &str = "class C {\n\
        \x20   companion { var p = \"FAIL\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   C::p.set(\"OK\")\n\
        \x20   return (C::p)()\n\
        }\n";
    assert_eq!(run(SRC).expect("property reference"), "OK");
}

#[test]
fn kotlinc_resolves_block_members_from_krusty_metadata() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       fun foo() = \"O\"\n\
        \x20       var bar: String = \"\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "fun box(): String {\n\
        \x20   A.bar = \"K\"\n\
        \x20   return A.foo() + A.bar\n\
        }\n";
    let classes = common::expect_classes_with_stdlib(&format!("{LANGUAGE}{LIB}"), "Lib");
    let dir = common::scratch_dir().expect("scratch directory");
    let lib = dir.join("lib");
    for (name, bytes) in &classes {
        let path = lib.join(format!("{name}.class"));
        std::fs::create_dir_all(path.parent().expect("class directory")).expect("mkdir");
        std::fs::write(&path, bytes).expect("write class file");
    }
    let main = dir.join("Main.kt");
    std::fs::write(&main, MAIN).expect("write main");
    let args = [
        "-d".to_string(),
        dir.join("out").to_string_lossy().into_owned(),
        "-XXLanguage:+CompanionBlocksAndExtensions".to_string(),
        "-cp".to_string(),
        lib.to_string_lossy().into_owned(),
        main.to_string_lossy().into_owned(),
    ];
    let Some((code, stderr)) = common::kotlinc_compile(&args) else {
        return;
    };
    assert_eq!(
        code, 0,
        "kotlinc rejected krusty's class metadata: {stderr}"
    );
}
