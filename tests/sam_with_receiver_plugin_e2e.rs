//! kotlinc's sam-with-receiver compiler plugin, as krusty's native pass.
//!
//! `-Xplugin=sam-with-receiver-compiler-plugin.jar
//! -P plugin:org.jetbrains.kotlin.samWithReceiver:annotation=<fqname>` makes a lambda converted to
//! an interface that carries the annotation take the abstract method's first parameter as its
//! receiver (`this`) rather than as `it`. Only an annotation the interface declaring the method
//! carries itself counts: not a meta-annotation, not one on a subinterface. Java and Kotlin
//! interfaces alike, through a SAM-converted argument and a SAM constructor. Every class krusty
//! emits must equal the reference compiler's byte for byte, and `box()` must return `OK`.

use std::path::PathBuf;

use super::common;
use super::compiler_plugin_fixture::{assert_same_classes_and_box, plugin_switches, PluginFixture};

const SAM_WITH_RECEIVER_ID: &str = "org.jetbrains.kotlin.samWithReceiver";

fn sam_with_receiver_switches(options: &[&str]) -> Vec<String> {
    plugin_switches(
        "sam-with-receiver-compiler-plugin.jar",
        SAM_WITH_RECEIVER_ID,
        options,
    )
}

/// Java interfaces, built by javac for both compilers: an annotated `Action<T>`, an unannotated
/// `Plain<T>`, and static methods that call them.
fn java_library() -> PathBuf {
    let sources = [
        (
            "WithReceiver.java",
            "package j;\n\
             @java.lang.annotation.Retention(java.lang.annotation.RetentionPolicy.RUNTIME)\n\
             public @interface WithReceiver {}\n",
        ),
        (
            "Action.java",
            "package j;\n@WithReceiver\npublic interface Action<T> { void execute(T target); }\n",
        ),
        (
            "Plain.java",
            "package j;\npublic interface Plain<T> { void execute(T target); }\n",
        ),
        (
            "Runner.java",
            "package j;\n\
             public final class Runner {\n\
             \x20   public static <T> void run(T target, Action<T> action) { action.execute(target); }\n\
             \x20   public static <T> void plain(T target, Plain<T> action) { action.execute(target); }\n\
             }\n",
        ),
    ]
    .map(|(name, text)| (name.to_string(), text.to_string()));
    common::javac_compile(&sources, &[])
        .map(|(dir, _)| dir)
        .expect("javac compiles the Java interfaces")
}

const MAIN: &str = r#"import j.*

@WithReceiver
annotation class Meta

@WithReceiver
fun interface Measure { fun measure(text: String): Int }

fun interface Inherited : Measure

@Meta
fun interface ViaMeta { fun measure(text: String): Int }

fun interface Unannotated { fun measure(text: String): Int }

@WithReceiver
fun interface AnnotatedOverride : Unannotated

@WithReceiver
fun interface Nothing0 { fun make(): Int }

@WithReceiver
fun interface Pair2 { fun combine(text: String, extra: Int): Int }

fun box(): String {
    var log = ""
    Runner.run("abc") { log += this + length }
    val action = Action<String> { log += length }
    action.execute("x")
    Runner.plain("p") { log += it }
    if (log != "abc31p") return "java=$log"
    if (Measure { length }.measure("four") != 4) return "measure"
    if (Inherited { length + 1 }.measure("four") != 5) return "inherited"
    if (ViaMeta { it.length }.measure("ab") != 2) return "meta"
    if (AnnotatedOverride { it.length }.measure("abc") != 3) return "override"
    if (Nothing0 { 7 }.make() != 7) return "nothing"
    if (Pair2 { extra -> length * extra }.combine("ab", 3) != 6) return "pair"
    return "OK"
}
"#;

/// Annotated Java and Kotlin interfaces, one inheriting its annotated method, and the shapes the
/// convention leaves alone: a meta-annotation, an annotated subinterface of an unannotated method,
/// a method with no parameters.
#[test]
fn sam_conversions_take_the_receiver_like_kotlinc() {
    let fixture = PluginFixture::new("sam-with-receiver");
    let library = java_library();
    let sources = [("Main.kt", MAIN)];
    let switches = sam_with_receiver_switches(&["annotation=j.WithReceiver"]);
    let classpath = [library.clone()];
    let reference = fixture.kotlinc("main", &sources, &classpath, &switches);
    let krusty = fixture.krusty("main", &sources, &classpath, &switches);
    assert_same_classes_and_box(&reference, &krusty, &[common::stdlib_jar(), library]);
}
