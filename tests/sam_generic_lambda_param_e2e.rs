use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn generic_sam_parameter_binds_from_named_argument() {
    const SRC: &str = "class Message(val text: String)\n\
fun interface Reader<T> {\n\
    fun read(value: T): String\n\
}\n\
fun <T> readWith(value: T, reader: Reader<T>): String {\n\
    return reader.read(value)\n\
}\n\
fun box(): String = readWith(reader = { it.text }, value = Message(\"OK\"))\n";
    assert_eq!(run(SRC).expect("named generic SAM argument"), "OK");
}

#[test]
fn generic_sam_parameter_substitutes_nested_types() {
    const SRC: &str = "class Box<T>(val value: T)\n\
fun interface Reader<T> {\n\
    fun read(value: Box<T>): String\n\
}\n\
fun <T> readWith(value: Box<T>, reader: Reader<T>): String = reader.read(value)\n\
fun box(): String = readWith(Box(\"OK\")) { it.value }\n";
    assert_eq!(run(SRC).expect("nested generic SAM parameter"), "OK");
}

#[test]
fn generic_sam_parameter_respects_declared_bound() {
    const SRC: &str = "open class Named(val name: String)\n\
class Entry(name: String) : Named(name)\n\
fun interface Reader<T> {\n\
    fun read(value: T): String\n\
}\n\
fun <T : Named> readWith(value: T, reader: Reader<T>): String = reader.read(value)\n\
fun box(): String = readWith(Entry(\"OK\")) { it.name }\n";
    assert_eq!(run(SRC).expect("bounded generic SAM parameter"), "OK");
}

#[test]
fn generic_sam_input_is_known_before_its_result_is_inferred_from_the_lambda() {
    const SRC: &str = "class Message(val text: String)\n\
fun interface Mapper<T, R> {\n\
    fun map(value: T): R\n\
}\n\
fun <T, R> map(value: T, mapper: Mapper<T, R>): R = mapper.map(value)\n\
fun box(): String = map(Message(\"OK\")) { it.text }\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "SamInputBeforeResult"),
        "OK"
    );
}
