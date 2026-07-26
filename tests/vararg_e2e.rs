//! `vararg` parameters — the call site packs trailing arguments into a fresh array (`newarray` +
//! element stores) passed as the array parameter — plus `for (x in arr)` array iteration to consume
//! it. Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn vararg_and_array_iteration_run() {
    let src = "fun sum(vararg xs: Int): Int { var s = 0; for (x in xs) s += x; return s }\n\
fun concat(vararg ss: String): String { var r = \"\"; for (s in ss) r = r + s; return r }\n\
fun box(): String {\n\
if (sum(1, 2, 3, 4) != 10) return \"f1\"\n\
if (sum() != 0) return \"f2\"\n\
if (concat(\"a\", \"b\", \"c\") != \"abc\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "V");
}

#[test]
fn classpath_java_instance_object_varargs_element_wise() {
    // A classpath Java INSTANCE method with an `Object...` vararg (`Formatter.format(String, Object...)`)
    // called with trailing element args. The method's `ACC_VARARGS` flag must be read so element-wise
    // arguments pack into the array — previously only Kotlin `@Metadata` carried the vararg flag, so a
    // plain Java varargs method was rejected once the arg count exceeded its fixed-arity overloads
    // (`debug(String, Object)`, `debug(String, Object, Object)`), yielding "none of the candidates".
    let src = "import java.util.Formatter\n\
fun box(): String {\n\
val s = Formatter().format(\"%s-%s-%s\", \"a\", \"b\", \"c\").toString()\n\
return if (s == \"a-b-c\") \"OK\" else \"FAIL:$s\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "VarargsJavaInstance");
}
