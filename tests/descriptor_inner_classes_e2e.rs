//! A dependency's nested class that a class names only in a descriptor or a generic signature
//! still gets an `InnerClasses` row, as kotlinc writes it.
//!
//! kotlinc records a nested class whenever its type mapper maps a type naming it: a declared
//! parameter or result, a callee's signature, a type argument inside a generic signature. None of
//! those needs a `CONSTANT_Class`, so offering the resolver only the pool's class constants left
//! each of these facades without the row kotlinc lists.
use super::common;
use krusty::jvm::classreader::parse_class;

/// A Java library whose nested classes the Kotlin fixtures name in exactly one position each.
fn nested_class_library() -> std::path::PathBuf {
    let java = [(
        "Api.java".to_string(),
        "package jlib;\n\
         public class Api {\n\
         \x20   public static class Key {}\n\
         \x20   public static class Made {}\n\
         \x20   public static class Item {}\n\
         \x20   public static Made made() { return new Made(); }\n\
         }\n"
        .to_string(),
    )];
    common::javac_compile(&java, &[])
        .expect("javac compiles the nested-class library")
        .0
}

/// Compile `source` with kotlinc and krusty against the Java library, and require `class` to be
/// byte-identical and to list exactly `rows` (inner names, in table order) in `InnerClasses`.
fn assert_identical_with_rows(stem: &str, source: &str, rows: &[&str]) {
    let library = nested_class_library();
    let class = format!("{stem}Kt");
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("fixture source");
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        "-cp".to_string(),
        library.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");
    let expected =
        std::fs::read(reference.join(format!("{class}.class"))).expect("kotlinc emits the facade");

    let classpath = [library, common::stdlib_jar()];
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(source, stem, &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{stem}: krusty rejected the fixture: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
            )
        });
    let (_, actual) = classes
        .iter()
        .find(|(name, _)| *name == class)
        .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));

    let listed = |bytes: &[u8]| {
        parse_class(bytes)
            .expect("a parseable class")
            .inner_classes
            .into_iter()
            .map(|entry| entry.inner)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        listed(&expected),
        rows,
        "{stem}: kotlinc's InnerClasses rows"
    );
    assert_eq!(listed(actual), rows, "{stem}: krusty's InnerClasses rows");
    assert!(
        *actual == expected,
        "{stem}: {class} is byte-identical to kotlinc"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Api.Key` appears only in the declared parameter's descriptor.
#[test]
fn a_declared_parameter_type_gets_its_row() {
    assert_identical_with_rows(
        "DeclaredNested",
        "import jlib.Api\nfun declared(key: Api.Key?): Int = 1\n",
        &["jlib/Api$Key"],
    );
}

/// `Api.Made` appears only in the callee's descriptor: the facade's own result is `Any?`.
#[test]
fn a_callee_descriptor_gets_its_row() {
    assert_identical_with_rows(
        "CalleeNested",
        "import jlib.Api\nfun called(): Any? = Api.made()\n",
        &["jlib/Api$Made"],
    );
}

/// The erased descriptor is `(Ljava/util/List;)I`; `Api.Item` is named only by the signature.
#[test]
fn a_type_argument_in_a_signature_gets_its_row() {
    assert_identical_with_rows(
        "SignatureNested",
        "import jlib.Api\nfun generic(items: List<Api.Item>?): Int = 1\n",
        &["jlib/Api$Item"],
    );
}

/// `Map.Entry` is one of kotlinc's predefined metadata names, written as a predefined index rather
/// than a local string, and its JVM class `java/util/Map$Entry` gets its row.
#[test]
fn a_nested_builtin_is_a_predefined_metadata_name() {
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    common::byte_diff_against_kotlinc_cp(
        "DeclaredEntry",
        "fun only(e: Map.Entry<String, Int>?): Int = 1\nfun calls(): Int = only(null)\n",
        "DeclaredEntryKt",
        &classpath,
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|e| panic!("DeclaredEntryKt byte-identical to kotlinc: {e}"));
}
