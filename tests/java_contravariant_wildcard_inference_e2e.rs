//! A Java `? super A` wildcard on a Kotlin `in` type parameter, against kotlinc.
//!
//! Java's `R<? super A>` reaches Kotlin as `R<in A>`. When `R` declares `in O`, the use-site
//! projection is redundant: an `R<Any>` argument bounds `A` above by `Any`, exactly as for a plain
//! `R<A>`, so `A` stays free for the factory argument to fix it as `String`.

use std::path::PathBuf;

use super::common;

const LIBRARY: &str = "package lib\n\
interface Renderer<in O> { fun render(o: O): String }\n\
interface PlainRenderer<in O> : Renderer<O>\n\
object Renderers {\n\
    @JvmField val TO_STRING: PlainRenderer<Any> = object : PlainRenderer<Any> {\n\
        override fun render(o: Any) = \"<$o>\"\n\
    }\n\
}\n\
open class Factory<E, A>\n";

const JAVA: &str = "package lib;\n\
public class RendererMap {\n\
    public String rendered = \"\";\n\
    public <E, A> void put(Factory<E, A> factory, String message, Renderer<? super A> renderer) {\n\
        rendered = message + \"/two\";\n\
    }\n\
    public <A> void putOne(Factory<String, A> factory, Renderer<? super A> renderer) {\n\
        rendered = \"one\";\n\
    }\n\
}\n";

const MAIN: &str = "import lib.*\n\
val factory = Factory<String, String>()\n\
fun box(): String {\n\
    val map = RendererMap()\n\
    map.put(factory, \"{0}\", Renderers.TO_STRING)\n\
    if (map.rendered != \"{0}/two\") return map.rendered\n\
    map.putOne(factory, Renderers.TO_STRING)\n\
    if (map.rendered != \"one\") return map.rendered\n\
    val applied = RendererMap().also { it.put(factory, \"{0}\", Renderers.TO_STRING) }\n\
    return if (applied.rendered == \"{0}/two\") \"OK\" else applied.rendered\n\
}\n";

fn classpath() -> Vec<PathBuf> {
    let kotlin = common::kotlinc_lib_out(&[("Lib.kt", LIBRARY)])
        .expect("kotlinc must compile the Kotlin renderer library");
    let stdlib = common::stdlib_jar();
    let (java, _) = common::javac_compile(
        &[("RendererMap.java".into(), JAVA.into())],
        &[kotlin.clone(), stdlib.clone()],
    )
    .expect("javac must compile the Java renderer map");
    vec![java, kotlin, stdlib]
}

#[test]
fn a_java_super_wildcard_on_a_kotlin_in_parameter_leaves_the_variable_to_its_inputs() {
    let classpath = classpath();
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &classpath);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the fixture: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty rejected a kotlinc-valid fixture: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(MAIN, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "the accepted fixture must lower and emit: diagnostics={:?}",
                common::front_end_diagnostics(MAIN, &classpath, Some(jdk.as_path())),
            )
        });
    assert_eq!(
        common::run_box(&classes, "MainKt", &classpath).expect("box runner"),
        "OK"
    );
}
