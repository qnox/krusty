//! A function value converted to a Kotlin fun interface is wrapped in the class kotlinc generates
//! once per file and interface, `<Facade>$sam$<interface FQ name>$0`, which implements the
//! interface and `FunctionAdapter`. krusty converted it with an `invokedynamic` over a private
//! forwarding method, which failed verification when the method returned a value class.

use super::common;

const SRC: &str = "package p\n\
    @JvmInline value class A(val value: Int)\n\
    class Payload(val value: Int)\n\
    fun interface B { fun f(x: A): A }\n\
    fun interface I { fun run(x: Int): Payload }\n\
    fun interface G<T> { fun get(x: T): T }\n\
    fun interface U { fun run() }\n\
    class Outer { fun interface In { fun h(): Int } }\n\
    fun b(f: (A) -> A): B = B(f)\n\
    fun i(f: (Int) -> Payload): I = I(f)\n\
    class C { fun i(f: (Int) -> Payload): I = I(f) }\n\
    fun g(f: (Payload) -> Payload): G<Payload> = G(f)\n\
    fun u(f: () -> Unit): U = U(f)\n\
    fun o(f: () -> Int): Outer.In = Outer.In(f)\n";

#[test]
fn each_wrapper_class_matches_kotlinc() {
    for class in [
        "p/SamWrapperKt$sam$p_B$0",
        "p/SamWrapperKt$sam$p_I$0",
        "p/SamWrapperKt$sam$p_G$0",
        "p/SamWrapperKt$sam$p_U$0",
        "p/SamWrapperKt$sam$p_Outer_In$0",
    ] {
        assert_eq!(
            common::byte_diff_against_kotlinc_cp("SamWrapper", SRC, class, &[common::stdlib_jar()])
                .expect("reference kotlinc is provisioned"),
            Ok(()),
            "{class}"
        );
    }
}

#[test]
fn wrappers_of_one_function_are_equal_and_forward_to_it() {
    let src = format!(
        "{SRC}fun box(): String {{\n\
             val f: (A) -> A = {{ A(it.value + 1) }}\n\
             val first = b(f)\n\
             val second = b(f)\n\
             if (first != second || first.hashCode() != second.hashCode()) return \"fail\"\n\
             if (C().i {{ Payload(it) }}.run(1).value != 1) return \"fail member\"\n\
             return if (first.f(A(1)).value == 2) \"OK\" else \"fail forward\"\n\
         }}\n"
    );
    let runtime_src = src
        .strip_prefix("package p\n")
        .expect("runtime fixture package prefix");
    common::expect_box_same_as_kotlinc(runtime_src, "SamWrapperBehavior");
}

const SHAPES_SRC: &str = "package shapes\n\
class Payload(val value: Int)\n\
fun interface Extension { fun Payload.apply(n: Int): Payload }\n\
fun interface Base { fun number(): Any }\n\
fun interface Primitive : Base { override fun number(): Int }\n\
fun interface Suspended { suspend fun text(x: Payload): Payload }\n\
fun interface Ordinary { fun text(x: Int): Payload }\n\
fun interface High { fun run(a1: Int, a2: Int, a3: Int, a4: Int, a5: Int, a6: Int, a7: Int, a8: Int, a9: Int, a10: Int, a11: Int, a12: Int, a13: Int, a14: Int, a15: Int, a16: Int, a17: Int, a18: Int, a19: Int, a20: Int, a21: Int, a22: Int, a23: Int): Int }\n\
fun extension(f: Payload.(Int) -> Payload): Extension = Extension(f)\n\
fun primitive(f: () -> Int): Primitive = Primitive(f)\n\
fun suspended(f: suspend (Payload) -> Payload): Suspended = Suspended(f)\n\
inline fun inlined(noinline f: (Int) -> Payload): Ordinary = Ordinary(f)\n\
fun high(f: (Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int) -> Int): High = High(f)\n";

/// The conversion site is part of the contract: Kotlin function values construct their shared
/// wrapper. This also pins extension-receiver, primitive-bridge, suspend, and inline-enclosing
/// shapes on the generic wrapper route.
#[test]
fn conversion_sites_match_kotlinc() {
    let built = common::compare_with_kotlinc_plugin(
        "SamWrapperShapes",
        SHAPES_SRC,
        "shapes/SamWrapperShapesKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in [
        "extension(",
        "primitive(",
        "suspended(",
        "inlined(",
        "high(",
    ] {
        let reference = common::method_instructions(&built.reference, method);
        assert!(
            reference
                .iter()
                .any(|instruction| instruction.contains("new")),
            "kotlinc constructs a wrapper in {method}: {reference:?}"
        );
        assert_eq!(
            common::method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn every_supported_wrapper_shape_matches_kotlinc() {
    for class in [
        "shapes/SamWrapperShapesKt$sam$shapes_Extension$0",
        "shapes/SamWrapperShapesKt$sam$shapes_Primitive$0",
        "shapes/SamWrapperShapesKt$sam$shapes_Suspended$0",
        "shapes/SamWrapperShapesKt$sam$i$shapes_Ordinary$0",
        "shapes/SamWrapperShapesKt$sam$shapes_High$0",
    ] {
        let built = common::compare_with_kotlinc_plugin(
            "SamWrapperShapes",
            SHAPES_SRC,
            class,
            &[common::stdlib_jar()],
            "1.8",
            &[],
        )
        .expect("reference kotlinc is provisioned");
        let class_header = |text: &str| {
            text.lines()
                .find(|line| line.contains(" implements "))
                .expect("wrapper class declaration")
                .to_string()
        };
        assert_eq!(
            class_header(&built.krusty),
            class_header(&built.reference),
            "{class} class declaration"
        );
        assert_eq!(
            common::member_blocks(&built.krusty),
            common::member_blocks(&built.reference),
            "{class} members"
        );
    }
}

const JAVA_CLASS_MODE_SRC: &str = "fun box(): String {\n\
    val f = { }\n\
    val class1 = (Runnable(f) as Object).getClass()\n\
    val class2 = (Runnable(f) as Object).getClass()\n\
    return if (class1 == class2) \"OK\" else \"$class1 $class2\"\n\
}\n";

/// `-Xsam-conversions=class` wraps two `Runnable` conversions of one function value in one class.
/// The class implements only `java.lang.Runnable`; the lambda literal itself stays `invokedynamic`.
#[test]
fn java_sam_class_mode_shares_one_wrapper() {
    common::expect_box_same_as_kotlinc_with_args(
        JAVA_CLASS_MODE_SRC,
        "JavaSamClassWrapper",
        &["-Xsam-conversions=class"],
    );

    let work = common::scratch_dir().expect("allocate Java SAM class-mode fixture");
    let source = work.join("Main.kt");
    std::fs::write(&source, JAVA_CLASS_MODE_SRC).expect("write fixture");
    let krusty_out = work.join("krusty");
    let reference_out = work.join("kotlinc");
    std::fs::create_dir_all(&krusty_out).expect("create krusty output");
    std::fs::create_dir_all(&reference_out).expect("create kotlinc output");
    let krusty = std::process::Command::new(common::krusty_binary())
        .args([
            "-d",
            krusty_out.to_str().expect("UTF-8 output"),
            "-no-reflect",
        ])
        .args(["-Xsam-conversions=class"])
        .arg(&source)
        .output()
        .expect("run krusty");
    assert!(
        krusty.status.success(),
        "krusty rejected class-mode Java SAM: {}",
        String::from_utf8_lossy(&krusty.stderr)
    );
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_out.to_string_lossy().into_owned(),
        "-Xsam-conversions=class".to_string(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected class-mode Java SAM: {stderr}");

    let wrapper = "MainKt$sam$java_lang_Runnable$0";
    let disassemble = |dir: &std::path::Path, class: &str| {
        common::javap(&["-p", "-c", "-v", "-cp", &dir.to_string_lossy(), class])
            .unwrap_or_else(|| panic!("javap {class}"))
    };
    let krusty_wrapper = disassemble(&krusty_out, wrapper);
    let reference_wrapper = disassemble(&reference_out, wrapper);
    let header = |text: &str| {
        text.lines()
            .find(|line| line.contains(" implements "))
            .expect("wrapper class declaration")
            .to_string()
    };
    assert_eq!(header(&krusty_wrapper), header(&reference_wrapper));
    assert!(
        !header(&krusty_wrapper).contains("FunctionAdapter"),
        "a Java SAM wrapper implements only the Java interface: {}",
        header(&krusty_wrapper)
    );
    assert_eq!(
        common::member_blocks(&krusty_wrapper),
        common::member_blocks(&reference_wrapper)
    );
    let facade = "MainKt";
    assert_eq!(
        common::method_instructions(&disassemble(&krusty_out, facade), "box("),
        common::method_instructions(&disassemble(&reference_out, facade), "box(")
    );
    let _ = std::fs::remove_dir_all(work);
}

/// Java SAM declarations do not take the Kotlin fun-interface wrapper route. The conversion keeps
/// the `invokedynamic` that asks LambdaMetafactory to implement the selected Java interface.
#[test]
fn java_sam_keeps_indy() {
    let java = [(
        "Task.java".to_string(),
        "package control; public interface Task { void run(); }".to_string(),
    )];
    let (java_sam, _) = common::javac_compile(&java, &[])
        .expect("javac must build the repository-owned Java SAM fixture");
    const JAVA_SRC: &str = "fun convert(f: () -> Unit): control.Task = control.Task(f)\n";
    let classpath = [common::stdlib_jar(), java_sam];
    let built = common::compare_with_kotlinc_plugin(
        "JavaSamControl",
        JAVA_SRC,
        "JavaSamControlKt",
        &classpath,
        "1.8",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let instructions = common::method_instructions(&built.krusty, "convert(");
    assert_eq!(
        instructions,
        common::method_instructions(&built.reference, "convert("),
        "Java SAM conversion must match kotlinc's LambdaMetafactory site"
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| instruction.contains("invokedynamic"))
            .count(),
        1,
        "Java SAM conversion must stay on LambdaMetafactory: {instructions:?}"
    );
}

const CONTEXT_SRC: &str = "// LANGUAGE: +ContextParameters\n\
package contextshape\n\
class Context\n\
class Value\n\
class Output\n\
fun interface Contextual { context(prefix: Context) fun text(value: Value): Output }\n\
fun contextual(f: context(Context) (Value) -> Output): Contextual = Contextual(f)\n";

#[test]
fn context_parameter_wrapper_matches_kotlinc() {
    let language = vec!["-XXLanguage:+ContextParameters".to_string()];
    let facade = common::compare_with_kotlinc_plugin(
        "ContextSamWrapper",
        CONTEXT_SRC,
        "contextshape/ContextSamWrapperKt",
        &[common::stdlib_jar()],
        "25",
        &language,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        common::method_instructions(&facade.krusty, "contextual("),
        common::method_instructions(&facade.reference, "contextual("),
    );

    let wrapper = common::compare_with_kotlinc_plugin(
        "ContextSamWrapper",
        CONTEXT_SRC,
        "contextshape/ContextSamWrapperKt$sam$contextshape_Contextual$0",
        &[common::stdlib_jar()],
        "1.8",
        &language,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        common::member_blocks(&wrapper.krusty),
        common::member_blocks(&wrapper.reference),
    );
}

const SUSPEND_VIEW_SRC: &str = "package adapt\n\
class Token\n\
fun interface SomeSuspend { suspend fun myInvoke(): Any? }\n\
fun interface Collector { suspend fun emit(value: Token) }\n\
fun interface Fn : (Token) -> Unit { override fun invoke(value: Token) }\n\
fun takeSuspend(f: SomeSuspend): String = \"s\"\n\
fun takeCollector(c: Collector): String = \"c\"\n\
fun fromProperty(p: kotlin.reflect.KProperty0<Any?>): String = takeSuspend(p)\n\
fun fromPropertyConstructor(p: kotlin.reflect.KProperty0<Any?>): String = takeSuspend(SomeSuspend(p))\n\
fun fromFun(f: Fn): String = takeCollector(f)\n\
fun fromFunction(f: (Token) -> Unit): String = takeCollector(f)\n\
val number: Int = 1\n\
fun box(): String {\n\
    try { fromProperty(::number) } catch (e: Throwable) { return \"property \" + e.javaClass.name }\n\
    try { fromPropertyConstructor(::number) } catch (e: Throwable) { return \"constructor \" + e.javaClass.name }\n\
    try { fromFun(object : Fn { override fun invoke(value: Token) {} }) } catch (e: Throwable) { return \"fun \" + e.javaClass.name }\n\
    try { fromFunction {} } catch (e: Throwable) { return \"function \" + e.javaClass.name }\n\
    return \"OK\"\n\
}\n";

/// A non-suspend callable view adapted to a suspend fun interface is stored as that view's
/// `FunctionN`. `KProperty0` is `Function0`, and `Fn : (Token) -> Unit` is `Function1`; neither
/// is checkcast to the suspend carrier `Function{n+1}`.
#[test]
fn non_suspend_view_keeps_its_function_arity_under_a_suspend_sam() {
    let runtime_src = SUSPEND_VIEW_SRC
        .strip_prefix("package adapt\n")
        .expect("runtime fixture package prefix");
    common::expect_box_same_as_kotlinc(runtime_src, "SuspendSamViewBehavior");
    let classes = common::expect_classes_with_stdlib(SUSPEND_VIEW_SRC, "SuspendSamView");
    let facade = classes
        .iter()
        .find(|(name, _)| name == "adapt/SuspendSamViewKt")
        .map(|(_, bytes)| bytes)
        .expect("facade");
    let work = common::scratch_dir().expect("scratch");
    let path = work.join("SuspendSamViewKt.class");
    std::fs::write(&path, facade).expect("write facade");
    let text = common::javap(&["-p", "-c", &path.to_string_lossy()]).expect("javap");
    let function_casts = |method: &str| {
        let mut lines = text.lines().map(str::trim);
        lines.find(|line| {
            line.split('(')
                .next()
                .and_then(|head| head.split_whitespace().last())
                == Some(method)
        });
        let mut casts = Vec::new();
        for line in lines {
            if line.contains('(') && line.ends_with(';') {
                break;
            }
            if line.contains("checkcast") {
                if let Some(class) = line.rsplit_once("// class ").map(|(_, class)| class) {
                    if class.starts_with("kotlin/jvm/functions/Function") {
                        casts.push(class.to_string());
                    }
                }
            }
        }
        casts
    };
    assert_eq!(
        function_casts("fromProperty"),
        ["kotlin/jvm/functions/Function0"],
        "a KProperty0 adapted as an argument is Function0"
    );
    assert_eq!(
        function_casts("fromPropertyConstructor"),
        ["kotlin/jvm/functions/Function0"],
        "a KProperty0 passed to the SAM constructor is Function0"
    );
    assert_eq!(
        function_casts("fromFun"),
        ["kotlin/jvm/functions/Function1"],
        "a non-suspend fun interface adapted to a suspend collector is Function1"
    );
    assert!(
        function_casts("fromFunction").is_empty(),
        "a non-suspend function value is already Function1: {:?}",
        function_casts("fromFunction")
    );
    let _ = std::fs::remove_dir_all(work);
}
