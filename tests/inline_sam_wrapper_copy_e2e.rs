//! A library's inline function that converts a function value to a Java interface constructs the
//! library's SAM wrapper (`LibKt$sam$i$java_util_function_Supplier$0`). kotlinc's inliner copies
//! that class into the calling class as `MainKt$inlined$sam$i$java_util_function_Supplier$0`,
//! once per calling class, replaying it with only its name remapped: the original's `@Metadata`,
//! `EnclosingMethod` and member order stay. krusty rejected the reified call shape (`assertThrows`
//! with a message has it) because the wrapper has no line numbers to map. A call that also inlines
//! a lambda the body invokes goes through the method inliner, which copies the wrapper the same way.
use super::common;

const LIB: &str = "package lib\n\
    import java.util.function.Supplier\n\
    fun <T> check(type: Class<T>, message: Supplier<String>): String = type.simpleName + message.get()\n\
    inline fun <reified T : Any> expect(noinline message: () -> String): String = check(T::class.java, message)\n";

const INVOKING_LIB: &str = "package lib\n\
    import java.util.function.Supplier\n\
    fun <T> check(type: Class<T>, message: Supplier<String>): String = type.simpleName + message.get()\n\
    inline fun <reified T : Any> expect(text: String, block: () -> Unit): String {\n\
    \x20   block()\n\
    \x20   val message: () -> String = { text }\n\
    \x20   return check(T::class.java, message)\n\
    }\n";

const INVOKING_MAIN: &str = "fun box(): String {\n\
    \x20   var ran = 0\n\
    \x20   val first = lib.expect<String>(\"!\") { ran += 1 }\n\
    \x20   val second = lib.expect<Int>(\"?\") { ran += 2 }\n\
    \x20   return if (first == \"String!\" && second == \"Integer?\" && ran == 3) \"OK\" else \"fail $first $second $ran\"\n\
    }\n";

const MAIN: &str = "fun box(): String {\n\
    \x20   val first = lib.expect<String> { \"!\" }\n\
    \x20   val second = lib.expect<Int> { \"?\" }\n\
    \x20   return if (first == \"String!\" && second == \"Integer?\") \"OK\" else \"fail $first $second\"\n\
    }\n";

#[test]
fn an_inlined_sam_wrapper_is_copied_once_as_kotlinc_copies_it() {
    assert_copies_the_wrapper_as_kotlinc("inline_sam_wrapper_copy", LIB, MAIN);
}

#[test]
fn a_call_inlining_its_lambda_copies_the_wrapper_as_kotlinc_copies_it() {
    assert_copies_the_wrapper_as_kotlinc(
        "inline_sam_wrapper_copy_invoking",
        INVOKING_LIB,
        INVOKING_MAIN,
    );
}

/// Builds `lib` with krusty, runs `main`'s box, and byte-compares the caller, the copied wrapper
/// and the class list with kotlinc's.
fn assert_copies_the_wrapper_as_kotlinc(name: &str, lib: &str, main: &str) {
    let library = common::compile_lib(name, lib).expect("krusty builds the library");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [library.clone(), stdlib.clone()];
    assert_eq!(
        common::compile_and_run_box(main, "Main", &classpath, Some(jdk.as_path())).as_deref(),
        Some("OK")
    );

    let root = common::scratch_dir().expect("scratch dir");
    let source = root.join("Main.kt");
    std::fs::write(&source, main).expect("write the caller");
    let krusty = root.join("krusty");
    common::compile_to_dir(main, "Main", &classpath, Some(jdk.as_path()), &krusty)
        .expect("krusty compiles the caller");
    let reference = root.join("kotlinc");
    let (code, stderr) = common::kotlinc_compile(&[
        "-cp".to_string(),
        format!("{}:{}", library.display(), stdlib.display()),
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc compiles the caller: {stderr}");
    for class in [
        "MainKt.class",
        "MainKt$inlined$sam$i$java_util_function_Supplier$0.class",
    ] {
        let read = |dir: &std::path::Path| {
            std::fs::read(dir.join(class)).unwrap_or_else(|error| panic!("{class}: {error}"))
        };
        assert!(
            read(&krusty) == read(&reference),
            "{class} differs from kotlinc"
        );
    }
    let classes = |dir: &std::path::Path| {
        let mut names = std::fs::read_dir(dir)
            .expect("list classes")
            .map(|entry| entry.expect("class entry").file_name())
            .filter(|name| name.to_string_lossy().ends_with(".class"))
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(classes(&krusty), classes(&reference));
    let _ = std::fs::remove_dir_all(root);
}

/// A caller compiled for a newer JVM target writes its copy of an older library's wrapper at the
/// caller's class-file version, as kotlinc writes every class it regenerates.
#[test]
fn an_inlined_sam_wrapper_copy_takes_the_callers_class_file_version() {
    let library =
        common::compile_lib("inline_sam_wrapper_copy", LIB).expect("krusty builds the library");
    let classpath = [library, common::stdlib_jar()];
    for class in [
        "MainKt",
        "MainKt$inlined$sam$i$java_util_function_Supplier$0",
    ] {
        let compared =
            common::compare_with_kotlinc_plugin("Main", MAIN, class, &classpath, "25", &[])
                .expect("reference kotlinc and javap must be available");
        assert!(
            compared.krusty_bytes == compared.reference_bytes,
            "{class} differs\n--- kotlinc ---\n{}\n--- krusty ---\n{}",
            compared.reference,
            compared.krusty
        );
    }
}
