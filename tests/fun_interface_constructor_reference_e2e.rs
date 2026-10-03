//! A `fun interface` constructor reference is a `FunInterfaceConstructorReference`, so it is a
//! `KFunction`, stays equal across files and type arguments, and still builds the SAM wrapper.

use super::common;

fn carrier_extends_fun_interface_constructor(source: &str, stem: &str) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(source, stem, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{stem} did not compile"));
    let carriers = classes
        .iter()
        .filter_map(|(name, bytes)| {
            let info = krusty::jvm::classreader::parse_class(bytes).expect("class file");
            info.super_class
                .is_some_and(|super_class| {
                    super_class.matches("kotlin/jvm/internal/FunInterfaceConstructorReference")
                })
                .then_some((name.clone(), info))
        })
        .collect::<Vec<_>>();
    assert_eq!(carriers.len(), 1, "{stem} carriers: {carriers:?}");
    let (_, info) = &carriers[0];
    assert!(
        info.interfaces
            .iter()
            .any(|interface| interface.matches("kotlin/jvm/functions/Function1")),
        "{stem} interfaces: {:?}",
        info.interfaces
    );
    assert!(
        info.methods
            .iter()
            .any(|method| method.name == "<init>" && method.descriptor == "()V"),
        "{stem} methods: {:?}",
        info.methods
            .iter()
            .map(|method| format!("{}{}", method.name, method.descriptor))
            .collect::<Vec<_>>()
    );
}

#[test]
fn implicit_kfunction_constructor_reference_invokes() {
    const SOURCE: &str = "fun interface KRunnable {\n\
         fun run()\n\
         }\n\
         val kr = ::KRunnable\n\
         fun box(): String {\n\
             var test = \"Failed\"\n\
             kr { test = \"OK\" }.run()\n\
             return test\n\
         }\n";
    carrier_extends_fun_interface_constructor(SOURCE, "FunIfaceCtor");
    common::expect_box_ok_with_stdlib(SOURCE, "FunIfaceCtor");
}

#[test]
fn fun_interface_constructor_reference_is_a_kfunction() {
    const SOURCE: &str = "import kotlin.reflect.KFunction\n\
         fun interface KRunnable {\n\
             fun run()\n\
         }\n\
         val kr = ::KRunnable\n\
         fun box(): String = if (kr is KFunction<*>) \"OK\" else \"Fail\"\n";
    common::expect_box_ok_with_stdlib(SOURCE, "FunIfaceKFunction");
}

#[test]
fn fun_interface_constructor_references_compare_by_interface_class() {
    const FIRST: &str = "fun interface KRunnable { fun run() }\n\
         val ks1: (() -> String) -> KSupplier<String> = ::KSupplier\n\
         val ks2: (() -> String) -> KSupplier<String> = ::KSupplier\n\
         val kn1: (() -> Number) -> KSupplier<Number> = ::KSupplier\n\
         fun checkEqual(message: String, a1: Any, a2: Any) {\n\
             if (a1 != a2) throw Exception(message)\n\
             if (a1.hashCode() != a2.hashCode()) throw Exception(message)\n\
         }\n\
         fun checkNotEqual(message: String, a1: Any, a2: Any) {\n\
             if (a1 == a2) throw Exception(message)\n\
         }\n\
         fun box(): String {\n\
             checkEqual(\"same file\", ks1, ks2)\n\
             checkEqual(\"other file\", ks1, ks3)\n\
             checkEqual(\"type arguments\", ks1, kn1)\n\
             val kr: (() -> Unit) -> KRunnable = ::KRunnable\n\
             checkNotEqual(\"other interface\", ks1, kr)\n\
             return \"OK\"\n\
         }\n";
    const SECOND: &str = "fun interface KSupplier<T> { fun get(): T }\n\
         val ks3: (() -> String) -> KSupplier<String> = ::KSupplier\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("funInterfaceConstructorEquality.kt", FIRST),
            ("KSupplier.kt", SECOND),
        ],
        "FunIfaceEquality",
    );
}
