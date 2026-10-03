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
fn contravariant_intersection_sam_runs() {
    const SRC: &str = "interface Top\n\
interface Common : Top\n\
abstract class BaseClass : Common\n\
interface BaseInterface : Common\n\
class ConcreteType : BaseClass(), BaseInterface\n\
class ConcreteType2 : BaseClass(), BaseInterface\n\
fun interface Consumer<T : Top> { fun accept(t: T) }\n\
class GenericHolder<T : Top> {\n\
    fun doOnSuccess(onSuccess: Consumer<in T>) {\n\
        onSuccess.accept(object : BaseClass() {} as T)\n\
    }\n\
}\n\
fun functionReference(x: Any) {}\n\
fun box(): String {\n\
    val instance = when (0) {\n\
        0 -> GenericHolder<ConcreteType>()\n\
        else -> GenericHolder<ConcreteType2>()\n\
    }\n\
    instance.doOnSuccess {}\n\
    instance.doOnSuccess(::functionReference)\n\
    return \"OK\"\n\
}\n";
    assert_eq!(common::kotlinc_box_result(SRC), "OK");
    assert_eq!(run(SRC).expect("contravariant intersection SAM"), "OK");
}

#[test]
fn unrelated_intersection_sam_runs() {
    const SRC: &str = "interface Top\n\
interface Unrelated\n\
interface A : Top, Unrelated\n\
interface B : Top, Unrelated\n\
fun interface IFoo<T : Top> { fun accept(t: T) }\n\
class G<T : Top> {\n\
    fun check(x: IFoo<in T>) { x.accept(object : A {} as T) }\n\
}\n\
fun functionReference(x: Any) {}\n\
fun box(): String {\n\
    val g = when (\"\".length) {\n\
        0 -> G<A>()\n\
        else -> G<B>()\n\
    }\n\
    g.check {}\n\
    g.check(::functionReference)\n\
    return \"OK\"\n\
}\n";
    assert_eq!(common::kotlinc_box_result(SRC), "OK");
    assert_eq!(run(SRC).expect("unrelated intersection SAM"), "OK");
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
