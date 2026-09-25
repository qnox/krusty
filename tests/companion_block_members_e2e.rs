//! `companion { … }` block members are static members of the classifier that declares the block:
//! kotlinc places their functions, accessors and backing fields on that class (initialized by the
//! class's `<clinit>`) and records them in the class's `@Metadata`, while a written
//! `companion fun C.f()` stays on the file facade.
use super::common;

const LANGUAGE: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n";

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(&format!("{LANGUAGE}{src}"), "Main")
}

/// Compile `src` with kotlinc (the feature enabled by flag) and krusty, and require each of
/// `classes` to carry exactly kotlinc's fields and methods, in class-file order with their access
/// flags, descriptors and generic signatures, and exactly kotlinc's `@Metadata`.
fn assert_members_and_metadata_match_kotlinc(stem: &str, src: &str, classes: &[&str]) {
    assert_members_and_metadata_match_kotlinc_on(stem, src, classes, &[common::stdlib_jar()]);
}

/// [`assert_members_and_metadata_match_kotlinc`] over `classpath`, also requiring each field's
/// `ConstantValue`. Returns the comparisons for further checks.
fn assert_members_and_metadata_match_kotlinc_on(
    stem: &str,
    src: &str,
    classes: &[&str],
    classpath: &[std::path::PathBuf],
) -> Vec<common::ReferenceComparison> {
    let source = format!("{LANGUAGE}{src}");
    let mut comparisons = Vec::new();
    for class in classes {
        let comparison = common::compare_with_kotlinc_plugin(
            stem,
            &source,
            class,
            classpath,
            "17",
            &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
        )
        .expect("reference kotlinc and javap are provisioned");
        let members = |bytes: &[u8]| {
            let info = krusty::jvm::classreader::parse_class(bytes).expect("a readable class file");
            let fields = info
                .fields
                .iter()
                .map(|field| {
                    format!(
                        "field {:#06x} {} {} {:?} {:?}",
                        field.access,
                        field.name,
                        field.descriptor,
                        field.signature,
                        field.const_value
                    )
                })
                .collect::<Vec<_>>();
            let methods = info.methods.iter().map(|method| {
                format!(
                    "method {:#06x} {}{} {:?}",
                    method.access, method.name, method.descriptor, method.signature
                )
            });
            fields.into_iter().chain(methods).collect::<Vec<_>>()
        };
        assert_eq!(
            members(&comparison.krusty_bytes),
            members(&comparison.reference_bytes),
            "{class}: kotlinc's member table"
        );
        assert_eq!(
            common::raw_kotlin_metadata(&comparison.krusty_bytes),
            common::raw_kotlin_metadata(&comparison.reference_bytes),
            "{class}: kotlinc's @Metadata"
        );
        comparisons.push(comparison);
    }
    comparisons
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
    assert_members_and_metadata_match_kotlinc("BlockMembers", SRC, &["A", "BlockMembersKt"]);
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
        fun initialize(): String {\n\
        \x20   initialized = true\n\
        \x20   return \"\"\n\
        }\n\
        class Foo {\n\
        \x20   companion { val p = initialize() }\n\
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
    assert_members_and_metadata_match_kotlinc("NestedBlock", SRC, &["Outer$Nested"]);
    assert_eq!(run(SRC).expect("nested block"), "OK");
}

#[test]
fn block_property_beside_companion_object_property_keeps_its_field_name() {
    const SRC: &str = "class E {\n\
        \x20   companion { val value = \"O\" }\n\
        \x20   companion object { val value = \"K\" }\n\
        }\n\
        fun box() = E.value + E.Companion.value\n";
    assert_members_and_metadata_match_kotlinc("FieldNames", SRC, &["E"]);
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

#[test]
fn library_block_members_are_statics_of_their_class() {
    const LIB: &str = "var initialized = false\n\
        fun initialize(): String { initialized = true; return \"\" }\n\
        open class A {\n\
        \x20   companion {\n\
        \x20       fun foo() = \"O\"\n\
        \x20       var bar: String = initialize()\n\
        \x20       const val SUFFIX = \"!\"\n\
        \x20   }\n\
        }\n";
    const MAIN: &str = "class B : A() {\n\
        \x20   companion { fun own() = \"K\" }\n\
        }\n\
        fun box(): String {\n\
        \x20   if (initialized) return \"a companion block initialized before its class\"\n\
        \x20   A.bar = B.own()\n\
        \x20   if (!initialized) return \"writing A.bar did not initialize A\"\n\
        \x20   return if (A.SUFFIX == \"!\") A.foo() + A.bar else \"A.SUFFIX is \" + A.SUFFIX\n\
        }\n";
    let result = common::expect_box_run_against(
        "companion-block-library",
        &format!("{LANGUAGE}{LIB}"),
        &format!("{LANGUAGE}{MAIN}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

/// A library block's `const val` is a compile-time constant where it is used: kotlinc folds `A.N`
/// into another `const val`'s `ConstantValue`, into an annotation argument and into an ordinary
/// expression, and none of them reads a field of `A`.
#[test]
fn library_block_const_is_a_compile_time_constant() {
    const LIB: &str = "class A {\n\
        \x20   companion {\n\
        \x20       const val N = 7\n\
        \x20   }\n\
        }\n\
        annotation class Tag(val n: Int)\n";
    const MAIN: &str = "const val M = A.N + 1\n\
        @Tag(A.N) fun tagged() {}\n\
        fun read(): Int = A.N\n\
        fun box(): String = if (M == 8 && read() == 7) \"OK\" else \"M is \" + M\n";
    let library = common::kotlinc_library(&format!("{LANGUAGE}{LIB}"))
        .expect("reference kotlinc is provisioned");
    let comparisons = assert_members_and_metadata_match_kotlinc_on(
        "LibraryConst",
        MAIN,
        &["LibraryConstKt"],
        &[library, common::stdlib_jar()],
    );
    let comparison = &comparisons[0];
    for method in ["read()", "tagged()"] {
        assert_eq!(
            common::method_instructions(&comparison.krusty, method),
            common::method_instructions(&comparison.reference, method),
            "LibraryConstKt.{method}: kotlinc's instructions"
        );
    }
    let annotations = |javap: &str| {
        javap
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("Tag("))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        annotations(&comparison.krusty),
        annotations(&comparison.reference),
        "tagged's annotation argument"
    );
}
