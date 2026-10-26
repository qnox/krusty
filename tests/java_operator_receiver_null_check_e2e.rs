//! An operator convention's receiver is the argument of the selected extension's receiver
//! parameter. A Java value committed there (`map[k] = v` through `MutableMap.set`, `list += x`
//! through `plusAssign`, `map + pair` through `Map.plus`) is checked like a written extension call's
//! receiver: a `null` from Java fails at the receiver with kotlinc's `name(...) must not be null`.

use super::common;

const JAVA: &str = "package p;\n\
    import java.util.*;\n\
    public class J {\n\
        public static Map<String, Integer> map() { return null; }\n\
        public static List<String> list() { return null; }\n\
    }\n";

const SOURCE: &str = "import p.J\n\
fun failure(block: () -> Unit): String =\n\
    try { block(); \"none\" } catch (e: NullPointerException) { e.message ?: \"null\" }\n\
fun box(): String {\n\
    val set = failure { J.map()[\"k\"] = 1 }\n\
    val add = failure { J.list() += \"x\" }\n\
    val plus = failure { J.map() + (\"a\" to 1) }\n\
    val local = failure { val m = J.map(); m[\"k\"] = 2 }\n\
    return \"$set|$add|$plus|$local\"\n\
}\n";

const EXPECTED: &str =
    "map(...) must not be null|list(...) must not be null|map(...) must not be null|null";

#[test]
fn a_java_operator_receiver_is_checked_like_kotlinc() {
    let (library, _) = common::javac_compile(&[("J.java".to_string(), JAVA.to_string())], &[])
        .expect("javac must compile the operator receiver fixture");
    let classpath = [library, common::stdlib_jar()];
    assert_eq!(
        common::kotlinc_box_result_with_classpath(SOURCE, &classpath[..1]),
        EXPECTED
    );
    let jdk = common::jdk_modules();
    let compiled = common::compile_in_process(SOURCE, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "the fixture must lower and emit: {:?}",
                common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()))
            )
        });
    assert_eq!(
        common::run_box(&compiled, "MainKt", &classpath).expect("box runner"),
        EXPECTED
    );
}
