//! A star projection and an `out` projection of the same readable bound denote one type argument:
//! `Resp<*>` is `Resp<out Any?>`, and a Java wildcard `Resp<?>` reaches Kotlin as `Resp<out Any!>`.
//! Inside an INVARIANT outer argument (`Pub<Resp<*>>` against the Java `Pub<Resp<?>>`) the two
//! spellings must therefore compare equal, in both directions; krusty compared projection kinds
//! literally and rejected `Publisher<MutableHttpResponse<*>>` against a Java
//! `Publisher<MutableHttpResponse<?>>` (Micronaut's `ServerFilterChain.proceed`) whichever way it
//! was written. Measured against kotlinc 2.4.10: every shape here compiles and runs.

use super::common;

const PUB_JAVA: &str = "public interface Pub<T> { T get(); }\n";
const RESP_JAVA: &str = "public interface Resp<B> { B body(); }\n";
const CHAIN_JAVA: &str = "public interface Chain {\n\
    Pub<Resp<?>> proceed(String request);\n\
    void install(Pub<Resp<?>> handler);\n\
}\n";

/// `java_interop_box` with the stdlib beside the Java fixture.
fn java_box_with_stdlib(tag: &str, java_sources: &[(&str, &str)], use_src: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let sources: Vec<(String, String)> = java_sources
        .iter()
        .map(|(name, source)| ((*name).to_string(), (*source).to_string()))
        .collect();
    let (cp, _) = common::javac_compile(&sources, &[])
        .unwrap_or_else(|| panic!("{tag}: pooled javac failed on the Java fixture"));
    let out = common::scratch_dir().unwrap_or_else(|| panic!("{tag}: scratch unavailable"));
    let classpath = [cp.clone(), stdlib.clone()];
    if common::compile_to_dir(use_src, "Use", &classpath, Some(jdk.as_path()), &out).is_none() {
        let diagnostics = common::front_end_diagnostics(use_src, &classpath, Some(jdk.as_path()));
        panic!("{tag}: krusty failed against the Java fixture; diagnostics: {diagnostics:?}");
    }
    let main =
        "public class M { public static void main(String[] a) { System.out.println(UseKt.box()); } }";
    let driver = out.join("M.java");
    std::fs::write(&driver, main).unwrap_or_else(|e| panic!("{tag}: write driver: {e}"));
    let run_cp = format!("{}:{}:{}", out.display(), cp.display(), stdlib.display());
    common::javac_run(
        &driver.to_string_lossy(),
        &run_cp,
        &out.to_string_lossy(),
        "M",
    )
    .unwrap_or_else(|| panic!("{tag}: pooled JavaRunner unavailable"))
    .trim()
    .to_string()
}

#[test]
fn a_star_projection_matches_a_java_wildcard_inside_an_invariant_argument() {
    let use_src = "class Stub<T> { infix fun returns(value: T): Stub<T> = this }\n\
fun <T> stub(block: () -> T): Stub<T> = Stub()\n\
class Body(val text: String) : Resp<String> { override fun body(): String = text }\n\
class Boxed(val value: Resp<*>) : Pub<Resp<*>> { override fun get(): Resp<*> = value }\n\
class Impl : Chain {\n\
    var installed: Pub<Resp<*>>? = null\n\
    override fun proceed(request: String): Pub<Resp<*>> = Boxed(Body(request))\n\
    override fun install(handler: Pub<Resp<*>>) { installed = handler }\n\
}\n\
fun box(): String {\n\
    val chain: Chain = Impl()\n\
    val direct: Pub<Resp<*>> = chain.proceed(\"r\")\n\
    val stubbed = stub { chain.proceed(\"s\") } returns Boxed(Body(\"b\")) as Pub<Resp<*>>\n\
    chain.install(Boxed(Body(\"i\")) as Pub<Resp<*>>)\n\
    val body = direct.get().body()\n\
    return if (body == \"r\" && stubbed === stubbed && (chain as Impl).installed != null) \"OK\" else \"FAIL: $body\"\n\
}\n";
    assert_eq!(
        java_box_with_stdlib(
            "star-projection-wildcard",
            &[
                ("Pub.java", PUB_JAVA),
                ("Resp.java", RESP_JAVA),
                ("Chain.java", CHAIN_JAVA)
            ],
            use_src
        ),
        "OK"
    );
}
