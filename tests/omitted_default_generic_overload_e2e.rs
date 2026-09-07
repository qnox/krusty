//! ktor's `HttpClient(CIO)`: a generic top-level function `fun <T : Config> HttpClient(engineFactory:
//! EngineFactory<T>, block: HttpConfig<T>.() -> Unit = {})` called with the defaulted lambda
//! omitted, beside a same-named class with a defaulted constructor parameter and a one-parameter
//! overload in a second facade. The top-level picker admitted the omitted-parameter shape as
//! applicable but never SELECTED it (only exact-arity and vararg shapes were ranked), the `$default`
//! bridge fallback lost among two same-named bridges, and Pass 1 then tried the constructor and
//! declined: every `val client = HttpClient(CIO)` lost its type and every use cascaded. Measured
//! against kotlinc 2.4.10 with the real ktor 3.5.1 jars; pinned here with a krusty-compiled library
//! of the same shape.

use super::common;

const LIB: &str = "package lib2\n\
open class EngineConfig\n\
class CioConfig : EngineConfig()\n\
interface Engine\n\
interface EngineFactory<out T : EngineConfig>\n\
object CIO : EngineFactory<CioConfig>\n\
class HttpConfig<T : EngineConfig> { var expectSuccess: Boolean = false }\n\
class HttpClient(val engine: Engine, val config: HttpConfig<out EngineConfig> = HttpConfig<CioConfig>()) {\n\
    fun close() {}\n\
}\n\
fun <T : EngineConfig> HttpClient(engineFactory: EngineFactory<T>, block: HttpConfig<T>.() -> Unit = {}): HttpClient =\n\
    HttpClient(object : Engine {}, HttpConfig<T>().apply(block))\n\
fun HttpClient(engine: Engine, block: HttpConfig<*>.() -> Unit): HttpClient = HttpClient(engine)\n";
const JVM: &str = "package lib2\n\
fun HttpClient(block: HttpConfig<*>.() -> Unit): HttpClient = HttpClient(object : Engine {})\n";

#[test]
fn a_generic_overload_with_an_omitted_defaulted_lambda_is_picked_over_the_constructor() {
    let Some(lib) = common::compile_libs(
        "omitted-default-generic-overload",
        &[("Lib.kt", LIB), ("Jvm.kt", JVM)],
    ) else {
        panic!("the library must compile");
    };
    let use_src = "import lib2.HttpClient\n\
import lib2.CIO\n\
class Holder {\n\
    val plain = HttpClient(CIO)\n\
    val configured = HttpClient(CIO) { expectSuccess = true }\n\
    fun close() = plain.close()\n\
}\n\
fun box(): String {\n\
    val holder = Holder()\n\
    holder.close()\n\
    return if (!holder.plain.config.expectSuccess && holder.configured.config.expectSuccess) \"OK\" else \"FAIL\"\n\
}\n";
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let classpath = [lib.clone(), stdlib.clone()];
    let diagnostics = common::front_end_diagnostics(use_src, &classpath, Some(jdk.as_path()));
    assert_eq!(diagnostics, Vec::<String>::new());
    let out = common::scratch_dir().expect("scratch");
    common::compile_to_dir(use_src, "Use", &classpath, Some(jdk.as_path()), &out)
        .expect("the use site must compile against the library");
    let main =
        "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let driver = out.join("M.java");
    std::fs::write(&driver, main).expect("write driver");
    let run_cp = format!("{}:{}:{}", out.display(), lib.display(), stdlib.display());
    let output = common::javac_run(
        &driver.to_string_lossy(),
        &run_cp,
        &out.to_string_lossy(),
        "M",
    )
    .expect("pooled JavaRunner");
    assert_eq!(output.trim(), "OK");
}
