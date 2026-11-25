//! kotlinc's constant pool follows the order its writer visits a method: the header first (ASM's
//! `visitMethod` interns the name and descriptor), then the body in evaluation order, then the
//! local-variable table.
//!
//! - The compiler-generated methods follow the header rule: a class's `$default` stub, the
//!   `$DefaultImpls` holder's forward to the interface's stub, and an interface's `access$…$jd`
//!   bridge each put their own name and descriptor ahead of what their bodies introduce.
//! - A constructor stores a body property after evaluating its initializer, so whatever the
//!   initializer references (a class, a constructor, a lambda) precedes the field's own entries,
//!   and the constructor's `this` follows them.
//!
//! - A primary constructor's descriptor covers every argument, a plain (non-property) parameter
//!   included, and its `$default` overload's body interns before the data-class members that follow.
//!
//! - An attribute name interns with the first method that uses it, `Code` included: an interface
//!   whose first method is abstract interns that method's `Signature` and annotation names before
//!   the `Code` of a later default body.
//!
//! - An inherited interface forwarder interns its header, nullability annotations included, before
//!   its `invokespecial` body.
//! - An anonymous object's constructor interns its `this` local before the members that follow it.
//!   A safe call in the next method does not move the constructor's parameter locals out of that
//!   range: they belong to the constructor, not to the method that follows it.
//! - A sealed class's nested subclasses intern with its `InnerClasses` table, after `@Metadata`,
//!   not ahead of its constructor.
//!
//! - A local declared in a nested block interns with the method's local-variable table, after
//!   every constant of the body, not where its block closes.
//! - An enum's declared functions intern before its synthesized `values`/`valueOf`/`getEntries`,
//!   and its fields, entry constants included, intern where a body first references them.
//! - An enum declaring only secondary constructors interns each one's header and locals at its own
//!   place; there is no synthesized primary to visit ahead of them.
//! - An enum's `$values` helper interns after its lifted local functions, which kotlinc visits first.
//! - A descriptor a removed unboxing interned in one method is placed where a later member's header
//!   first names it, after what earlier code already interned.
//! - A constant a removed null check interned in one method is placed where a later method
//!   first names it, even when a later rewritten method names it too.
//! - An enum's property accessors intern at their declarations: a getter's name does not precede
//!   the setter of an earlier property.
//! - A data class's declared functions intern before its synthesized `componentN`/`copy`/
//!   `toString`/`hashCode`/`equals`, whose bodies and local-variable tables intern where they are
//!   emitted.
//! - A constant a rewrite removed from one method's body interns where a later body first names
//!   it: an always-true type check leaves no class entry ahead of the next method, a discarded
//!   `Unit` read's field interns after a lambda body's header, and an object's elided `INSTANCE`
//!   read interns its name at `<clinit>`'s store, since fields are written after methods.
//! - A data object's synthesized `equals` interns its `this` and `other` locals with its body,
//!   before `<clinit>`.
//!
//! Each case asserts that the named classes are byte-identical to kotlinc's. The fixtures use
//! neutral names only.
use super::common;

/// Compile `src` with kotlinc and with krusty against `classpath` and return both builds of each
/// class in `classes`.
fn build_both(
    stem: &str,
    src: &str,
    classes: &[&str],
    classpath: &[std::path::PathBuf],
) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    let source = dir.join(format!("{stem}.kt"));
    std::fs::write(&source, src).expect("write fixture");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let krusty =
        common::compile_in_process_metadata_cp_module_target(src, stem, classpath, "main", None)
            .expect("krusty compiles the fixture");
    let pairs = classes
        .iter()
        .map(|class| {
            let reference = std::fs::read(reference_dir.join(format!("{class}.class")))
                .expect("kotlinc emits the class");
            let ours = krusty
                .iter()
                .find(|(internal, _)| internal == class)
                .map(|(_, bytes)| bytes.clone())
                .expect("krusty emits the class");
            (class.to_string(), reference, ours)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    pairs
}

fn assert_identical(stem: &str, src: &str, classes: &[&str]) {
    assert_identical_against(stem, src, classes, &[]);
}

/// [`assert_identical`] with the standard library on krusty's classpath, for a fixture whose
/// shape depends on what the library's metadata declares.
fn assert_identical_with_stdlib(stem: &str, src: &str, classes: &[&str]) {
    assert_identical_against(stem, src, classes, &[common::stdlib_jar()]);
}

fn assert_identical_against(
    stem: &str,
    src: &str,
    classes: &[&str],
    classpath: &[std::path::PathBuf],
) {
    for (class, reference, krusty) in build_both(stem, src, classes, classpath) {
        assert!(reference == krusty, "{class} differs from kotlinc's build");
    }
}

#[test]
fn a_member_default_stub_interns_its_header_before_its_body() {
    assert_identical(
        "MemberStub",
        "class Meter {\n\
         \x20   fun read(scale: Int, unit: String = \"mm\"): String = unit + scale\n\
         }\n",
        &["Meter"],
    );
}

#[test]
fn a_holder_forward_interns_its_header_before_its_body() {
    assert_identical(
        "HolderForward",
        "interface Dial {\n\
         \x20   fun turn(by: Int = 1): Int = by\n\
         }\n",
        &["Dial$DefaultImpls", "Dial"],
    );
}

#[test]
fn a_property_initializer_interns_its_value_before_its_field() {
    assert_identical(
        "InitializerOrder",
        "class Part\n\
         class Holder(val size: Int) {\n\
         \x20   val part: Part = Part()\n\
         \x20   val action: () -> Int = { 7 }\n\
         \x20   val big: Long = 123456789012L\n\
         }\n",
        &["Part", "Holder"],
    );
}

/// Data-class fields are visited with the other declared fields, after the constructor, so a
/// generic field's `Signature` follows the constructor's `this`.
#[test]
fn a_data_class_generic_field_signature_follows_its_constructor() {
    assert_identical(
        "DataFieldSignature",
        "class Box<T>(val item: T)\n\
         data class Crate(val box: Box<String>)\n",
        &["Box", "Crate"],
    );
}

#[test]
fn a_plain_constructor_parameter_is_in_the_constructor_header() {
    assert_identical(
        "PlainParameter",
        "open class Base(p: Int)\n\
         class Cell<T>(t: T) {\n\
         \x20   var value = t\n\
         }\n",
        &["Base", "Cell"],
    );
}

#[test]
fn a_default_constructor_body_interns_before_data_members() {
    assert_identical(
        "DefaultBody",
        "data class Pair2(val a: Int = 1, val b: String = \"$a\")\n",
        &["Pair2"],
    );
}

/// A plain parameter carries its `@NotNull` parameter annotation, interned with the constructor's
/// header, and its null check runs before the super call.
#[test]
fn a_plain_constructor_parameter_is_annotated_and_checked_before_the_super_call() {
    assert_identical(
        "PlainParameterCheck",
        "open class Base(val s: String)\n\
         class Derived(label: String, n: Int) : Base(label + n)\n",
        &["Base", "Derived"],
    );
}

/// A plain non-null reference parameter interns its `@NotNull` with the constructor's header and
/// its `checkNotNullParameter` name with the constructor's body, although it has no field.
#[test]
fn a_plain_reference_constructor_parameter_is_annotated_with_the_header() {
    assert_identical("PlainReference", "class Label(text: String)\n", &["Label"]);
}

/// A parameter's annotations are placed by its constructor position, not by the position of the
/// stored field that follows it.
#[test]
fn an_annotated_plain_parameter_beside_a_stored_field_keeps_its_position() {
    assert_identical(
        "AnnotatedBesideField",
        "annotation class Mark\n\
         class Tagged(@Mark text: String, val count: Int)\n\
         class Counted(val count: Int, @Mark text: String)\n",
        &["Mark", "Tagged", "Counted"],
    );
}

#[test]
fn an_abstract_first_method_interns_its_attribute_names_before_code() {
    assert_identical(
        "AbstractFirst",
        "interface Source<T> {\n\
         \x20   fun take(label: String): T\n\
         \x20   fun first(label: String): T = take(label)\n\
         }\n",
        &["Source"],
    );
}

#[test]
fn an_inherited_forwarder_interns_its_annotations_before_its_body() {
    assert_identical(
        "InheritedForwarder",
        "interface Greeter {\n\
         \x20   fun greet(name: String): String = name\n\
         }\n\
         class Plain : Greeter\n",
        &["Plain"],
    );
}

#[test]
fn a_sealed_class_interns_its_nested_subclasses_with_its_inner_classes() {
    assert_identical(
        "SealedRows",
        "sealed class Shape {\n\
         \x20   class Round : Shape()\n\
         \x20   class Square : Shape()\n\
         }\n",
        &["Shape"],
    );
}

/// A safe call in the method after the constructor (`eldest?.value?.close()`) is rewritten.
/// The constructor's `$receiver` and `$super_call_param$N` locals stay with the constructor,
/// ahead of that method. The member is an ordinary function so the comparison is the pool itself.
#[test]
fn an_anonymous_object_keeps_constructor_locals_when_the_next_method_is_rewritten() {
    let src = "import java.io.Closeable\n\
         class Host(private val limit: Int) {\n\
         \x20   private val factor = 0.75f\n\
         \x20   private val opened = object : java.util.LinkedHashMap<String, Closeable>(limit, factor, true) {\n\
         \x20       fun drop(eldest: MutableMap.MutableEntry<String, Closeable>?) {\n\
         \x20           eldest?.value?.close()\n\
         \x20       }\n\
         \x20   }\n\
         }\n";
    let classpath = [common::stdlib_jar()];
    let jdk = common::jdk_modules();
    let dir = common::scratch_dir().expect("scratch directory");
    let reference_dir = dir.join("ref");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    let source = dir.join("RewrittenNext.kt");
    std::fs::write(&source, src).expect("write fixture");
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let krusty = common::compile_in_process(src, "RewrittenNext", &classpath, Some(jdk.as_path()))
        .expect("krusty compiles the fixture");
    for class in ["Host", "Host$opened$1"] {
        let reference =
            std::fs::read(reference_dir.join(format!("{class}.class"))).expect("kotlinc class");
        let ours = krusty
            .iter()
            .find(|(name, _)| name == class)
            .map(|(_, bytes)| bytes.clone())
            .expect("krusty emits the class");
        assert_eq!(ours, reference, "{class} differs from kotlinc's build");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_anonymous_object_constructor_interns_this_before_its_members() {
    assert_identical(
        "AnonymousThis",
        "interface Greeter {\n\
         \x20   fun greet(name: String): String = name\n\
         }\n\
         fun make(): Greeter = object : Greeter {}\n",
        &["AnonymousThisKt$make$1"],
    );
}

#[test]
fn a_data_object_equals_interns_its_locals_with_its_body() {
    assert_identical("DataObjectLocals", "data object Marker\n", &["Marker"]);
}

/// An enum constructor interns every declared parameter's local with its own table, plain ones
/// included, before `values`.
#[test]
fn an_enum_constructor_interns_its_plain_parameter_locals() {
    assert_identical(
        "EnumPlainParameter",
        "enum class Level(weight: Int) {\n\
         \x20   LOW(1)\n\
         }\n",
        &["Level"],
    );
}

/// A nullable enum property's getter interns `@Nullable` with the declared members, before
/// `values`, as a non-null one interns `@NotNull`.
#[test]
fn a_nullable_enum_property_interns_its_annotation_with_the_declared_members() {
    assert_identical(
        "EnumNullableProperty",
        "enum class Node(val parent: Node?) {\n\
         \x20   ROOT(null),\n\
         \x20   LEAF(ROOT),\n\
         }\n",
        &["Node"],
    );
}

/// A companion `var` hoisted to its outer class keeps a setter on the companion, and that setter
/// interns its `<set-?>` local's descriptor with its own body, before the next member.
#[test]
fn a_hoisted_companion_setter_interns_its_locals_with_its_body() {
    assert_identical(
        "HoistedCompanionSetter",
        "class Holder {\n\
         \x20   companion object {\n\
         \x20       var label = \"x\"\n\
         \x20       fun touch() {}\n\
         \x20   }\n\
         }\n",
        &["Holder$Companion", "Holder"],
    );
}

/// ASM visits a method's local variables after all of its instructions, so a nested block's local
/// follows a string the method loads after that block has closed.
#[test]
fn a_nested_block_local_interns_after_the_whole_body() {
    assert_identical(
        "NestedBlockLocal",
        "fun pick(n: Long): String {\n\
         \x20   if (n > 0L) {\n\
         \x20       val wide = n + 1L\n\
         \x20       if (wide > 5L) return \"big\"\n\
         \x20   }\n\
         \x20   return \"small\"\n\
         }\n",
        &["NestedBlockLocalKt"],
    );
}

/// kotlinc visits an enum's declared functions before `values()`, and its fields after every
/// method, so a declared function's header and locals precede the synthesized members.
#[test]
fn an_enum_declared_function_interns_before_its_synthesized_members() {
    assert_identical(
        "EnumDeclaredFunction",
        "enum class Mode(val label: String) {\n\
         \x20   ON(\"on\"), OFF(\"off\");\n\
         \x20   val shout = \"!\"\n\
         \x20   fun pick(level: Int): Int = level\n\
         }\n",
        &["Mode"],
    );
}

/// A data class's synthesized `componentN`/`copy`/`toString`/`hashCode`/`equals` are emitted after
/// its declared functions, and they intern their pool entries there too.
#[test]
fn a_data_class_declared_function_interns_before_its_synthesized_members() {
    assert_identical(
        "DataDeclaredFunction",
        "data class Cell(val level: Int) {\n\
         \x20   fun merge(other: Cell): Cell = other\n\
         }\n",
        &["Cell"],
    );
}

#[test]
fn a_class_an_elided_type_check_named_interns_where_a_later_body_names_it() {
    assert_identical(
        "ElidedCheck",
        r#"class Marker

class Cell<T>(val value: T)

fun isCell(cell: Cell<out Any?>): Boolean = cell is Cell<*>

fun pick(first: Any): Any {
    val marker = first as Marker
    return Cell(marker)
}
"#,
        &["ElidedCheckKt"],
    );
}

#[test]
fn a_field_an_elided_read_named_interns_after_the_next_body_s_header() {
    assert_identical(
        "ElidedRead",
        r#"val action: () -> Unit = {}

fun run(): String {
    action() as Unit
    return "OK"
}
"#,
        &["ElidedReadKt"],
    );
}

#[test]
fn an_object_s_elided_instance_read_interns_its_name_at_the_static_initializer() {
    assert_identical_with_stdlib(
        "ElidedInstance",
        r#"fun bump(): String {
    Counter += 1
    return "OK"
}

object Counter {
    operator fun plusAssign(step: Any) = Unit
}
"#,
        &["Counter", "ElidedInstanceKt"],
    );
}

/// An enum declaring only secondary constructors has no primary constructor: kotlinc visits the
/// declared members in order, so a constructor's header interns after the property before it.
#[test]
fn an_enum_with_only_secondary_constructors_interns_them_at_their_declarations() {
    assert_identical(
        "EnumSecondaries",
        "enum class Gate {\n\
         \x20   Open(\"x\"), Shut;\n\
         \n\
         \x20   val label: String\n\
         \n\
         \x20   constructor(label: String) { this.label = label }\n\
         \x20   constructor() : this(\"\")\n\
         }\n",
        &["Gate"],
    );
}

/// A function-local lambda inside an enum member is lifted to a method that kotlinc visits before
/// the private `$values` helper.
#[test]
fn an_enum_values_helper_interns_after_its_lifted_local_functions() {
    assert_identical(
        "EnumLifted",
        "class Sink<T> { fun put(value: T) {} }\n\
         fun <R> build(init: Sink<R>.() -> Unit): Sink<R> = Sink<R>().apply(init)\n\
         enum class Phase {\n\
         \x20   Start;\n\
         \x20   fun run() {\n\
         \x20       fun inner() {\n\
         \x20           val phase: Phase = Start\n\
         \x20           build { put(phase) }\n\
         \x20       }\n\
         \x20       inner()\n\
         \x20   }\n\
         }\n",
        &["Phase"],
    );
}

/// `test`'s inlined `run` unboxes a value that kotlinc never writes, so the `()I` it interned moves
/// to the first header that names it, `hashCode()`, which follows `hashCode-impl`'s body.
#[test]
fn a_removed_unboxing_s_descriptor_interns_at_the_next_header_that_names_it() {
    assert_identical_with_stdlib(
        "RemovedUnboxing",
        "@JvmInline\n\
         value class Slot(private val raw: Int) {\n\
         \x20   fun test() = run { done() }\n\
         \n\
         \x20   private fun done() = \"OK\"\n\
         }\n\
         \n\
         fun probe() = Slot(0).test()\n",
        &["Slot"],
    );
}

/// `park`'s `as` interns an `Intrinsics.checkNotNull` that a later pass removes. `launch` is the
/// first body that still names `Intrinsics`, so kotlinc interns it there; a later rewritten method
/// (`run`, which also uses it through `!!`) must not keep it where `park` interned it.
#[test]
fn a_removed_null_check_s_class_interns_where_a_later_method_names_it() {
    assert_identical_with_stdlib(
        "RemovedNullCheck",
        "import kotlin.coroutines.*\n\
         \n\
         object Sink : Continuation<Any?> {\n\
             override val context: CoroutineContext = EmptyCoroutineContext\n\
             override fun resumeWith(result: Result<Any?>) { result.getOrThrow() }\n\
         }\n\
         \n\
         @JvmInline value class Tag(val s: String)\n\
         \n\
         var pending: Continuation<Any>? = null\n\
         \n\
         suspend fun <T> park(): T = suspendCoroutine {\n\
             @Suppress(\"UNCHECKED_CAST\")\n\
             pending = it as Continuation<Any>\n\
         }\n\
         \n\
         fun launch(body: suspend () -> Unit) {\n\
             body.startCoroutine(Sink)\n\
         }\n\
         \n\
         fun run(): String {\n\
             var out: String? = null\n\
             launch {\n\
                 out = \"x\"\n\
             }\n\
             pending?.resume(Tag(\"OK\"))\n\
             return out!!\n\
         }\n\
         \n",
        &["RemovedNullCheckKt"],
    );
}

/// Each accessor of an enum's primary-constructor properties interns at its own declaration, so a
/// getter follows the setter of the property before it.
#[test]
fn an_enum_s_property_accessors_intern_at_their_declarations() {
    assert_identical(
        "EnumAccessors",
        "enum class Gear(var speed: Int, var torque: Int = 1, val ratio: Int = 1, var limit: Int = 2) {\n\
         \x20   First(1)\n\
         }\n",
        &["Gear"],
    );
}
