//! An `inline` property accessor is spliced at the use, the same way an inline function is.
//! A reified `T::class` inside the accessor is therefore the call site's class. Calling the
//! erased accessor instead answers `Object` and the read returns the failure branch.
use super::common;

#[test]
fn inline_reified_extension_property_uses_the_call_site_class() {
    const SRC: &str = "\
class Token\n\
val <reified T> T.foo: Int\n\
    inline get() { return if (T::class.simpleName == \"Token\") 1 else -100 }\n\
inline var <reified T> T.bar: Int\n\
    get() { return if (T::class.simpleName == \"Token\") 2 else -100 }\n\
    set(v) { }\n\
fun box(): String = if (Token().foo + Token().bar == 3) \"OK\" else \"fail\"\n";
    common::expect_box_same_as_kotlinc(SRC, "inlineReifiedProperty");
}

#[test]
fn inline_reified_property_setter_uses_the_call_site_class() {
    const SRC: &str = "\
class Token\n\
var seen = 0\n\
inline var <reified T> T.note: Int\n\
    get() { return if (T::class.simpleName == \"Token\") 2 else -100 }\n\
    set(v) { seen = v + if (T::class.simpleName == \"Token\") 3 else -100 }\n\
fun box(): String {\n\
    val token = Token()\n\
    token.note = 4\n\
    return if (token.note == 2 && seen == 7) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "inlineReifiedPropertySetter");
}

/// A function-valued property reference still invokes the checker-selected inline getter. The
/// generated adapter is lowering machinery, not a second resolution site, so it must carry the
/// exact reified substitution published for the reference.
#[test]
fn an_inline_reified_property_reference_uses_the_checked_type_argument() {
    const SRC: &str = "\
class Token(val text: String)\n\
inline val <reified T> T.identity: T\n\
    get() = (this as Any) as T\n\
fun apply(read: (Token) -> Token): String = read(Token(\"OK\")).text\n\
fun box(): String = apply(Token::identity)\n";
    common::expect_box_same_as_kotlinc(SRC, "inlineReifiedPropertyReference");
}

/// The extension receiver and the stored value each run once. The setter sees that value.
#[test]
fn an_inline_setter_evaluates_its_receiver_and_value_once() {
    common::expect_box_same_as_kotlinc(
        "var log = \"\"\n\
         fun touch(label: String): String {\n\
             log += label\n\
             return \"v\"\n\
         }\n\
         inline var String.note: String\n\
             get() = \"g\"\n\
             set(value) { log += value }\n\
         fun box(): String {\n\
             touch(\"R\").note = touch(\"W\")\n\
             return if (log == \"RWv\") \"OK\" else log\n\
         }\n",
        "inlineSetterOnce",
    );
}

/// The dispatch receiver of a member extension is the selected instance, evaluated once.
#[test]
fn an_inline_member_extension_evaluates_its_dispatch_receiver_once() {
    common::expect_box_same_as_kotlinc(
        "class Host {\n\
             var log = \"\"\n\
             fun touch(): Host {\n\
                 log += \"D\"\n\
                 return this\n\
             }\n\
             inline var String.mark: String\n\
                 get() = \"g\"\n\
                 set(value) { log += value }\n\
         }\n\
         fun box(): String {\n\
             val host = Host()\n\
             with(host.touch()) { \"x\".mark = \"V\" }\n\
             return if (host.log == \"DV\") \"OK\" else host.log\n\
         }\n",
        "inlineDispatchOnce",
    );
}

/// An inline accessor that reads another inline accessor expands both at the use.
#[test]
fn a_nested_inline_accessor_uses_the_outer_type_argument() {
    common::expect_box_same_as_kotlinc(
        "inline val <reified T> T.inner: String\n\
             get() = if (T::class.simpleName == \"String\") \"K\" else \"fail\"\n\
         inline val <reified T> T.outer: String\n\
             get() = this.inner\n\
         fun box(): String = if (\"\".outer == \"K\") \"OK\" else \"fail\"\n",
        "nestedInlineAccessor",
    );
}

/// A `return` inside the accessor is the property read's value, including an early return.
#[test]
fn an_inline_getter_return_is_the_property_value() {
    common::expect_box_same_as_kotlinc(
        "inline val String.head: String\n\
             get() {\n\
                 if (isEmpty()) return \"empty\"\n\
                 return \"OK\"\n\
             }\n\
         fun box(): String = if (\"a\".head == \"OK\" && \"\".head == \"empty\") \"OK\" else \"fail\"\n",
        "inlineGetterReturn",
    );
}

/// A selected inline accessor declared in another file retains and materializes the same checked
/// body as a same-file use. Its reified operation therefore sees the caller's concrete type.
#[test]
fn an_inline_reified_property_in_another_file_is_spliced() {
    const LIB: &str = r#"
inline fun (Int.() -> String).foo(): String = this(1)

inline var (Int.() -> String).bar: String
    get() = this(1)
    set(value) { this(1) }

inline fun localFoo(): String {
    return object {
        fun func(): String {
            class C
            C()
            return "L"
        }
    }.func()
}

object Host {
    inline val <reified T> T.tag: Int
        get() = if (T::class.simpleName == "Token") 7 else -1
}

class Token
"#;
    const MAIN: &str = r#"
import Host.tag

fun box(): String {
    val text = { a: Int -> if (a == 1) "O" else "fail" }.foo() +
        { a: Int -> if (a == 1) "K" else "fail" }.bar
    if (localFoo() != "L") return "fail local"
    if (Token().tag != 7) return "fail tag"
    return text
}
"#;
    assert_eq!(
        run_box_files(&[("lib.kt", LIB), ("main.kt", MAIN)]),
        common::kotlinc_box_files_result(&[("lib.kt", LIB), ("main.kt", MAIN)], "MainKt")
    );
}

/// A cross-file inline extension property passes its function-typed receiver into a non-inline call.
#[test]
fn a_cross_file_inline_extension_property_keeps_its_receiver() {
    const LIB: &str = r#"
inline fun (Int.() -> String).foo(): String = noInlineRun(this)

inline var (Int.() -> String).bar: String
    get() = noInlineRun(this)
    set(value) { noInlineRun(this) }

fun noInlineRun(f: Int.() -> String): String = f(1)
"#;
    const MAIN: &str = r#"
fun box() = { a: Int -> if (a == 1) "O" else "FA" }.foo() + { a: Int -> if (a == 1) "K" else "IL" }.bar
"#;
    let sources = [("lib.kt", LIB), ("main.kt", MAIN)];
    assert_eq!(
        run_box_files(&sources),
        common::kotlinc_box_files_result(&sources, "MainKt")
    );
}

/// Local classes inside an anonymous object in a cross-file inline property are part of the splice.
#[test]
fn a_cross_file_inline_property_keeps_local_classes_in_an_anonymous_object() {
    const LIB: &str = r#"
inline fun foo(): String {
    return object {
        fun func(): String {
            class C
            C()
            return "O"
        }
    }.func()
}

inline val bar: String get() {
    return object {
        fun func(): String {
            class C
            C()
            return "K"
        }
    }.func()
}
"#;
    const MAIN: &str = "fun box(): String = foo() + bar\n";
    let sources = [("lib.kt", LIB), ("main.kt", MAIN)];
    assert_eq!(
        run_box_files(&sources),
        common::kotlinc_box_files_result(&sources, "MainKt")
    );
}

/// An imported member extension on an object keeps its dispatch receiver when spliced in another file.
#[test]
fn an_imported_object_member_extension_property_is_spliced() {
    const LIB: &str = r#"
package test

object A {
    inline fun <T> bar(x: T) = 42
    inline val <T> T.bar2 get() = 42
}
"#;
    const MAIN: &str = r#"
package test

import test.A.bar
import test.A.bar2

fun <T> T.foo1(): Int = bar(this)
fun <T> T.foo2(): Int = this.bar2

fun box(): String {
    10.foo1()
    10.foo2()
    return "OK"
}
"#;
    let sources = [("lib.kt", LIB), ("main.kt", MAIN)];
    assert_eq!(
        run_box_files(&sources),
        common::kotlinc_box_files_result(&sources, "test.MainKt")
    );
}

fn run_box_files(sources: &[(&str, &str)]) -> String {
    let jdk = common::jdk_modules();
    let classpath = [common::stdlib_jar()];
    if let Some(result) =
        common::compile_and_run_box_files(sources, &classpath, Some(jdk.as_path()))
    {
        return result;
    }
    let report = compile_files_diagnostics(sources, &classpath, Some(jdk.as_path()));
    panic!("cross-file inline property did not run: {report}");
}

fn compile_files_diagnostics(
    sources: &[(&str, &str)],
    cp_jars: &[std::path::PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> String {
    use krusty::diag::DiagSink;
    use krusty::source::SourceInput;
    let mut diags = DiagSink::new();
    let stems = sources
        .iter()
        .map(|(name, _)| name.trim_end_matches(".kt").to_string())
        .collect::<Vec<_>>();
    let inputs = sources
        .iter()
        .zip(&stems)
        .map(|((_, source), stem)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let cp = common::cached_classpath(cp_jars, jdk_modules);
    let platform =
        Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()).expect("JVM provider"));
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        common::with_native_plugins(platform),
        &krusty::features::LangFeatures::default(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let backend = krusty::jvm::JvmBackend::new(cp);
    let _ = krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diags);
    if diags.diags.is_empty() {
        return "lowering/emit bailed without a diagnostic".to_string();
    }
    diags
        .diags
        .iter()
        .map(|diagnostic| {
            format!(
                "file {} span {}..{}: {}",
                diagnostic.file, diagnostic.span.lo, diagnostic.span.hi, diagnostic.msg
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
