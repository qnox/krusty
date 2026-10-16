//! A Java generic method's `Signature` attribute names `byte` and `short` exactly as its descriptor
//! does. Declared through that signature they are `Byte`/`Short` (and `ByteArray`/`ShortArray`), as
//! kotlinc reads them, not the `Int` the JVM operand stack widens them to. Read as `Int`, a Kotlin
//! override of `<T> T readValue(byte[] bytes, Class<T> type)` overrode nothing.

use super::common;

const READER_JAVA: &str = "package fixtures;\n\
public interface Reader {\n\
    <T> T readValue(byte[] bytes, Class<T> type);\n\
    <T> T readValue(String text, Class<T> type);\n\
    <T> int sum(short[] values, char[] tags, T tag);\n\
    <T> int widen(byte b, short s, T tag);\n\
}\n";

const SOURCE: &str = "import fixtures.Reader

class Decoder : Reader {
    override fun <T : Any> readValue(bytes: ByteArray, type: Class<T>): T = type.cast(\"b\" + bytes.size)
    override fun <T : Any> readValue(text: String, type: Class<T>): T = type.cast(\"s\" + text)
    override fun <T> sum(values: ShortArray, tags: CharArray, tag: T): Int = values.sum() + tags.size
    override fun <T> widen(b: Byte, s: Short, tag: T): Int = b + s
}

fun box(): String {
    val reader: Reader = Decoder()
    if (reader.readValue(byteArrayOf(1, 2), String::class.java) != \"b2\") return \"bytes\"
    if (reader.readValue(\"x\", String::class.java) != \"sx\") return \"text\"
    if (reader.sum(shortArrayOf(3, 4), charArrayOf('a'), Unit) != 8) return \"sum\"
    return if (reader.widen(5, 6, Unit) == 11) \"OK\" else \"widen\"
}
";

#[test]
fn a_java_generic_method_keeps_its_byte_and_short_parameter_types() {
    let java = common::javac_compile(&[("Reader.java".to_string(), READER_JAVA.to_string())], &[])
        .expect("javac compiles the Java fixture")
        .0;
    let reference = common::kotlinc_box_result_with_classpath(SOURCE, std::slice::from_ref(&java));
    assert_eq!(reference, "OK", "kotlinc must accept and run the fixture");
    let jdk = common::jdk_modules();
    let classpath = [java, common::stdlib_jar()];
    let classes = common::compile_in_process(SOURCE, "Main", &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SOURCE, &classpath, Some(jdk.as_path()))
            )
        });
    let output = common::run_box(&classes, "MainKt", &classpath).expect("run krusty box");
    assert_eq!(output, reference);
}
