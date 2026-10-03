//! An escaping lambda created inside an inline function is a separate implementation.
//! A reified check in that implementation, including one nested inside another lambda, has to use
//! the call's type argument. An ordinary type parameter stays erased and keeps one implementation.
use super::common;
use std::path::Path;

fn collect_class_names(root: &Path, directory: &Path, names: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_class_names(root, &path, names);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            names.push(
                path.strip_prefix(root)
                    .expect("class below output root")
                    .with_extension("")
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

fn class_names_with_args(
    stem: &str,
    source: &str,
    shared_args: &[&str],
) -> (Vec<String>, Vec<String>) {
    let work = common::scratch_dir().expect("allocate lambda-name fixture");
    let source_path = work.join(format!("{stem}.kt"));
    let krusty_output = work.join("krusty");
    let reference_output = work.join("reference");
    std::fs::write(&source_path, source).expect("write lambda-name fixture");
    std::fs::create_dir_all(&krusty_output).expect("create krusty output");
    std::fs::create_dir_all(&reference_output).expect("create reference output");

    let krusty = std::process::Command::new(common::krusty_binary())
        .args(["-d", krusty_output.to_str().expect("UTF-8 output")])
        .arg("-no-reflect")
        .args(shared_args)
        .arg(&source_path)
        .output()
        .expect("run krusty");
    assert!(
        krusty.status.success(),
        "{stem}: krusty failed: {}",
        String::from_utf8_lossy(&krusty.stderr)
    );
    let mut reference_args = vec![
        "-d".to_string(),
        reference_output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
    ];
    reference_args.extend(shared_args.iter().map(|argument| (*argument).to_string()));
    reference_args.push(source_path.to_string_lossy().into_owned());
    let (code, stderr) =
        common::kotlinc_compile(&reference_args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let mut krusty_names = Vec::new();
    collect_class_names(&krusty_output, &krusty_output, &mut krusty_names);
    krusty_names.sort();
    let mut reference_names = Vec::new();
    collect_class_names(&reference_output, &reference_output, &mut reference_names);
    reference_names.sort();
    let _ = std::fs::remove_dir_all(work);
    (reference_names, krusty_names)
}

#[test]
fn an_escaping_reified_check_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token(val label: String) : Item\n\
         class Root(val label: String) : Item\n\
         var check: (Item) -> Boolean = { false }\n\
         inline fun <reified T : Item> defineFunc() {\n\
             check = { it is T }\n\
         }\n\
         fun box(): String {\n\
             defineFunc<Token>()\n\
             val token = Token(\"t\")\n\
             val root = Root(\"r\")\n\
             return if (check(token) && !check(root)) \"OK\" else \"Fail\"\n\
         }\n",
        "EscapingReifiedCheck",
    );
}

#[test]
fn a_nested_escaping_reified_check_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token(val label: String) : Item\n\
         class Root(val label: String) : Item\n\
         var check: (Item) -> Boolean = { false }\n\
         inline fun <reified T : Item> defineFunc() {\n\
             check = {\n\
                 val nested = { value: Item -> value is T }\n\
                 nested(it)\n\
             }\n\
         }\n\
         fun box(): String {\n\
             defineFunc<Token>()\n\
             val token = Token(\"t\")\n\
             val root = Root(\"r\")\n\
             return if (check(token) && !check(root)) \"OK\" else \"Fail\"\n\
         }\n",
        "NestedEscapingReifiedCheck",
    );
}

#[test]
fn a_cross_file_escaping_reified_safe_cast_preserves_its_null_check() {
    const LIB: &str = "\
interface Item\n\
class Token : Item\n\
class Other : Item\n\
var inspect: (Item) -> Unit = {}\n\
var rejected = false\n\
inline fun <reified T : Item> install() {\n\
    inspect = { value ->\n\
        val candidate = value as? T\n\
        if (candidate == null) rejected = true\n\
    }\n\
}\n";
    const MAIN: &str = "\
fun box(): String {\n\
    install<Token>()\n\
    inspect(Token())\n\
    if (rejected) return \"retained value was rejected\"\n\
    inspect(Other())\n\
    return if (rejected) \"OK\" else \"failed cast lost its null check\"\n\
}\n";
    let sources = [("CastDeclaration.kt", LIB), ("CastCaller.kt", MAIN)];
    let reference = common::kotlinc_box_files_result(&sources, "CastCallerKt");
    assert_eq!(reference, "OK", "the reference fixture succeeds");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("compile and run the checked cross-file safe cast"),
        reference,
    );
}

#[test]
fn nested_specialized_lambda_class_names_keep_source_nesting() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineNested() {\n\
    check = {\n\
        val nested = { value: Item -> value is T }\n\
        nested(it)\n\
    }\n\
}\n\
fun nestedBox(): String {\n\
    defineNested<Token>()\n\
    return if (check(Token())) \"OK\" else \"Fail\"\n\
}\n";
    let (reference, krusty) = class_names_with_args("NestedReifiedClass", source, &[]);
    assert_eq!(krusty, reference);
    assert!(reference
        .iter()
        .any(|class| class == "NestedReifiedClassKt$defineNested$1"));
    assert!(reference
        .iter()
        .any(|class| class == "NestedReifiedClassKt$defineNested$1$nested$1"));
    assert!(reference
        .iter()
        .any(|class| class == "NestedReifiedClassKt$nestedBox$$inlined$defineNested$1$1"));
}

#[test]
fn a_local_function_caller_keeps_its_lifted_path() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun outer() {\n\
    fun localCall() {\n\
        defineFunc<Token>()\n\
    }\n\
    localCall()\n\
}\n";
    let classes = [
        "ReifiedLocalCallerKt",
        "Item",
        "Token",
        "ReifiedLocalCallerKt$defineFunc$1",
        "ReifiedLocalCallerKt$outer$localCall$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedLocalCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn an_indy_lambda_caller_keeps_its_physical_owner_and_lambda_segment() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun outer() {\n\
    val caller = { defineFunc<Token>() }\n\
    caller()\n\
}\n";
    let classes = [
        "ReifiedOrdinaryLambdaCallerKt",
        "Item",
        "Token",
        "ReifiedOrdinaryLambdaCallerKt$defineFunc$1",
        "ReifiedOrdinaryLambdaCallerKt$outer$lambda$0$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedOrdinaryLambdaCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn a_class_lambda_caller_uses_its_generated_owner_and_invoke_method() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun outer() {\n\
    val caller = { defineFunc<Token>() }\n\
    caller()\n\
}\n";
    let expected = [
        "Item",
        "ReifiedClassLambdaCallerKt",
        "ReifiedClassLambdaCallerKt$check$1",
        "ReifiedClassLambdaCallerKt$defineFunc$1",
        "ReifiedClassLambdaCallerKt$outer$caller$1",
        "ReifiedClassLambdaCallerKt$outer$caller$1$invoke$$inlined$defineFunc$1",
        "Token",
    ]
    .map(str::to_string);
    let (reference, krusty) =
        class_names_with_args("ReifiedClassLambdaCaller", source, &["-Xlambdas=class"]);
    assert_eq!(reference, expected);
    assert_eq!(krusty, reference);
}

#[test]
fn a_class_sam_caller_uses_the_selected_sam_method() {
    let source = "\
interface Item\n\
class Token : Item\n\
fun interface Runner { fun run() }\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun outer() {\n\
    val caller = Runner { defineFunc<Token>() }\n\
    caller.run()\n\
}\n";
    let expected = [
        "Item",
        "ReifiedClassSamCallerKt",
        "ReifiedClassSamCallerKt$defineFunc$1",
        "ReifiedClassSamCallerKt$outer$caller$1",
        "ReifiedClassSamCallerKt$outer$caller$1$run$$inlined$defineFunc$1",
        "Runner",
        "Token",
    ]
    .map(str::to_string);
    let (reference, krusty) = class_names_with_args(
        "ReifiedClassSamCaller",
        source,
        &["-Xsam-conversions=class"],
    );
    assert_eq!(reference, expected);
    assert_eq!(krusty, reference);
}

#[test]
fn non_function_callers_own_their_specialized_lambda_classes() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
val special = defineFunc<Token>()\n\
class Host {\n\
    val special = defineFunc<Token>()\n\
    fun update() {\n\
        defineFunc<Token>()\n\
    }\n\
}\n\
fun box(): String {\n\
    Host().update()\n\
    return if (check(Token())) \"OK\" else \"Fail\"\n\
}\n";
    let classes = [
        "ReifiedInitializerCallerKt",
        "Item",
        "Token",
        "Host",
        "ReifiedInitializerCallerKt$defineFunc$1",
        "ReifiedInitializerCallerKt$special$$inlined$defineFunc$1",
        "Host$special$$inlined$defineFunc$1",
        "Host$update$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedInitializerCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn getter_and_setter_callers_share_the_jvm_special_ordinal_group() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
class AccessorHost {\n\
    var accessorCall: Unit\n\
        get() { defineFunc<Token>() }\n\
        set(value) { defineFunc<Token>() }\n\
}\n";
    let classes = [
        "ReifiedAccessorCallerKt",
        "Item",
        "Token",
        "AccessorHost",
        "ReifiedAccessorCallerKt$defineFunc$1",
        "AccessorHost$special$$inlined$defineFunc$1",
        "AccessorHost$special$$inlined$defineFunc$2",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedAccessorCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn a_constructor_caller_uses_the_jvm_special_segment() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
class ConstructorHost {\n\
    constructor(value: Int) {\n\
        defineFunc<Token>()\n\
    }\n\
}\n";
    let classes = [
        "ReifiedConstructorCallerKt",
        "Item",
        "Token",
        "ConstructorHost",
        "ReifiedConstructorCallerKt$defineFunc$1",
        "ConstructorHost$special$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedConstructorCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn an_init_block_caller_uses_the_jvm_special_segment() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
class InitializerHost {\n\
    init {\n\
        defineFunc<Token>()\n\
    }\n\
    init {\n\
        defineFunc<Token>()\n\
    }\n\
}\n";
    let classes = [
        "ReifiedInitCallerKt",
        "Item",
        "Token",
        "InitializerHost",
        "ReifiedInitCallerKt$defineFunc$1",
        "InitializerHost$special$$inlined$defineFunc$1",
        "InitializerHost$special$$inlined$defineFunc$2",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedInitCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn a_top_level_default_caller_uses_the_jvm_default_segment() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun withDefault(value: Unit = defineFunc<Token>()) {}\n";
    let classes = [
        "ReifiedTopDefaultCallerKt",
        "Item",
        "Token",
        "ReifiedTopDefaultCallerKt$defineFunc$1",
        "ReifiedTopDefaultCallerKt$withDefault$default$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedTopDefaultCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn a_member_default_caller_keeps_its_class_owner() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
class DefaultHost {\n\
    fun withDefault(value: Unit = defineFunc<Token>()) {}\n\
}\n";
    let classes = [
        "ReifiedMemberDefaultCallerKt",
        "Item",
        "Token",
        "DefaultHost",
        "ReifiedMemberDefaultCallerKt$defineFunc$1",
        "DefaultHost$withDefault$default$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedMemberDefaultCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn a_constructor_default_caller_uses_the_jvm_special_segment() {
    let source = "\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
class DefaultConstructorHost(value: Unit = defineFunc<Token>())\n";
    let classes = [
        "ReifiedConstructorDefaultCallerKt",
        "Item",
        "Token",
        "DefaultConstructorHost",
        "ReifiedConstructorDefaultCallerKt$defineFunc$1",
        "DefaultConstructorHost$special$$inlined$defineFunc$1",
    ];
    let pairs =
        common::compile_with_kotlinc("ReifiedConstructorDefaultCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn a_companion_block_function_keeps_its_class_owner() {
    let source = "\
// LANGUAGE: +CompanionBlocksAndExtensions\n\
interface Item\n\
class Token : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
class CompanionFunctionHost {\n\
    companion {\n\
        fun update() { defineFunc<Token>() }\n\
    }\n\
}\n";
    let classes = [
        "ReifiedCompanionCallerKt",
        "Item",
        "Token",
        "CompanionFunctionHost",
        "ReifiedCompanionCallerKt$defineFunc$1",
        "CompanionFunctionHost$update$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("ReifiedCompanionCaller", source, &[], &classes);
    assert_eq!(pairs.len(), classes.len());
}

#[test]
fn an_escaping_lambda_forwards_its_reified_argument() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token(val label: String) : Item\n\
         class Root(val label: String) : Item\n\
         var accepted: (Item) -> Boolean = { false }\n\
         inline fun <reified T : Item> accept(value: Item): Boolean = value is T\n\
         inline fun <reified T : Item> defineFunc() {\n\
             accepted = { accept<T>(it) }\n\
         }\n\
         fun box(): String {\n\
             defineFunc<Token>()\n\
             val token = Token(\"t\")\n\
             val root = Root(\"r\")\n\
             return if (accepted(token) && !accepted(root)) \"OK\" else \"Fail\"\n\
         }\n",
        "ForwardedReifiedLambda",
    );
}

#[test]
fn an_escaping_lambda_specializes_type_of() {
    common::expect_box_same_as_kotlinc(
        "import kotlin.reflect.typeOf\n\
         interface Item\n\
         class Token(val label: String) : Item\n\
         var kind: () -> Any = { Any() }\n\
         inline fun <reified T : Item> defineFunc() {\n\
             kind = { typeOf<T>() }\n\
         }\n\
         fun box(): String {\n\
             defineFunc<Token>()\n\
             return if (kind().toString().contains(\"Token\")) \"OK\" else \"Fail\"\n\
         }\n",
        "TypeOfReifiedLambda",
    );
}

#[test]
fn an_ordinary_parameter_keeps_the_shared_lambda_and_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "class Token(val label: String)\n\
         var latest: () -> Token? = { null }\n\
         inline fun <T> defineFunc(value: T) {\n\
             latest = { value as? Token }\n\
         }\n\
         fun box(): String {\n\
             defineFunc(Token(\"OK\"))\n\
             return latest()?.label ?: \"Fail\"\n\
         }\n",
        "OrdinaryEscapingLambda",
    );
}

#[test]
fn a_reified_sibling_does_not_reify_an_ordinary_parameter() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         var f: (Any) -> Any? = { null }\n\
         inline fun <T, reified R : Item> define() {\n\
             f = { value -> if (value is R) value as? T else null }\n\
         }\n\
         fun box(): String {\n\
             define<String, Token>()\n\
             return if (f(Token()) is Token) \"OK\" else \"Fail\"\n\
         }\n",
        "MixedReifiedLambda",
    );
}

#[test]
fn the_call_site_lambda_class_matches_kotlinc() {
    let source = "\
interface Item\n\
class Token(val label: String) : Item\n\
class Root(val label: String) : Item\n\
var check: (Item) -> Boolean = { false }\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { it is T }\n\
}\n\
fun box(): String {\n\
    defineFunc<Token>()\n\
    val token = Token(\"t\")\n\
    val root = Root(\"r\")\n\
    return if (check(token) && !check(root)) \"OK\" else \"Fail\"\n\
}\n";
    let classes = [
        "EscapingReifiedClassKt",
        "Item",
        "Token",
        "Root",
        "EscapingReifiedClassKt$defineFunc$1",
        "EscapingReifiedClassKt$box$$inlined$defineFunc$1",
    ];
    let pairs = common::compile_with_kotlinc("EscapingReifiedClass", source, &[], &classes);
    let inlined = pairs
        .iter()
        .zip(classes)
        .find(|(_, class)| class.ends_with("$$inlined$defineFunc$1"))
        .expect("the inlined class pair")
        .0;
    let implementation = b"defineFunc$lambda$0$box$1";
    for (label, bytes) in [("kotlinc", &inlined.0), ("krusty", &inlined.1)] {
        assert!(
            bytes.windows(6).any(|window| window == b"invoke"),
            "{label}: the call-site class implements invoke"
        );
        assert!(
            bytes.windows(5).any(|window| window == b"Token"),
            "{label}: the call-site class checks Token"
        );
        assert!(
            !bytes
                .windows(implementation.len())
                .any(|window| window == implementation),
            "{label}: the call-site class holds the body, not a facade implementation"
        );
    }
    let facade = pairs
        .iter()
        .zip(classes)
        .find(|(_, class)| *class == "EscapingReifiedClassKt")
        .expect("the file facade")
        .0;
    for (label, bytes) in [("kotlinc", &facade.0), ("krusty", &facade.1)] {
        assert!(
            bytes.windows(5).any(|window| window == b"Token"),
            "{label}: the facade names Token"
        );
        assert!(
            !bytes
                .windows(implementation.len())
                .any(|window| window == implementation),
            "{label}: the facade does not keep the lambda implementation"
        );
    }
}

const SPECIALIZED_SUSPEND_LAMBDA: &str = "\
import kotlin.coroutines.*\n\
interface Item\n\
class Token : Item\n\
class Root : Item\n\
var check: suspend (Item) -> Boolean = { false }\n\
suspend fun pause() {}\n\
inline fun <reified T : Item> defineFunc() {\n\
    check = { value ->\n\
        val nested: suspend (Item) -> Boolean = { candidate -> pause(); candidate is T }\n\
        nested(value)\n\
    }\n\
}\n\
fun box(): String {\n\
    fun install() { class Local; Local(); defineFunc<Token>() }\n\
    install()\n\
    var token = false\n\
    var root = true\n\
    check.startCoroutine(Token(), Continuation(EmptyCoroutineContext) { token = it.getOrThrow() })\n\
    check.startCoroutine(Root(), Continuation(EmptyCoroutineContext) { root = it.getOrThrow() })\n\
    return if (token && !root) \"OK\" else \"Fail\"\n\
}\n";

#[test]
fn a_specialized_suspend_lambda_has_the_call_site_class_and_invoke_suspend_override() {
    common::expect_box_same_as_kotlinc(SPECIALIZED_SUSPEND_LAMBDA, "ReifiedSuspendCaller");
    let (reference, krusty) =
        class_names_with_args("ReifiedSuspendCaller", SPECIALIZED_SUSPEND_LAMBDA, &[]);
    assert_eq!(krusty, reference);
    let specialized = "ReifiedSuspendCallerKt$box$install$$inlined$defineFunc$1";
    let nested = "ReifiedSuspendCallerKt$box$install$$inlined$defineFunc$1$1";
    let local = "ReifiedSuspendCallerKt$box$install$Local";
    assert!(krusty.iter().any(|class| class == specialized));
    assert!(krusty.iter().any(|class| class == nested));
    assert!(krusty.iter().any(|class| class == local));
    common::assert_same_inner_classes(
        "ReifiedSuspendCaller",
        SPECIALIZED_SUSPEND_LAMBDA,
        &[],
        &["ReifiedSuspendCallerKt", specialized, nested, local],
    );

    let classes =
        common::expect_classes_with_stdlib(SPECIALIZED_SUSPEND_LAMBDA, "ReifiedSuspendCaller");
    for expected in [specialized, nested] {
        let bytes = &classes
            .iter()
            .find(|(class, _)| class == expected)
            .expect("the specialized suspend lambda class")
            .1;
        let class = krusty::jvm::classreader::parse_class(bytes)
            .expect("the specialized suspend lambda class parses");
        let invoke_suspend = class
            .methods
            .iter()
            .filter(|method| method.name == "invokeSuspend")
            .map(|method| (method.name.as_str(), method.descriptor.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            invoke_suspend,
            [("invokeSuspend", "(Ljava/lang/Object;)Ljava/lang/Object;")]
        );
    }
}

#[test]
fn a_dependency_mixed_reified_signature_specializes_only_its_reified_parameter() {
    let library_source = "\
package external_reified\n\
interface Item\n\
class Token : Item\n\
class Root : Item\n\
@JvmInline value class Carrier(val item: Item)\n\
var check: (Item) -> Any? = { null }\n\
var ordinary: () -> String = { \"unset\" }\n\
inline fun <T, reified R : Item> define(value: Any) {\n\
    check = { item -> if (item is R) value as? T else null }\n\
}\n\
open class Host {\n\
    inline fun <T, reified R : Item> Item.defineMember(value: Any) {\n\
        check = { item -> if (item is R) value as? T else null }\n\
    }\n\
    inline fun <T, reified R : Item> Carrier.defineCarrier(value: Any) {\n\
        check = { candidate -> if (candidate is R && item is R) value as? T else null }\n\
    }\n\
}\n\
inline fun <T> defineOrdinary(value: T) { ordinary = { value.toString() } }\n\
fun result(): String {\n\
    val mixed = check(Token())\n\
    return if (mixed is Token && check(Root()) == null && ordinary() == \"plain\") \"OK\" else \"Fail\"\n\
}\n";
    let Some(library) = common::kotlinc_library(library_source) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let source = "\
import external_reified.*\n\
class ConsumerHost : Host() {\n\
    fun install() { Token().defineMember<Root, Token>(Token()) }\n\
    fun installCarrier() { Carrier(Token()).defineCarrier<Root, Token>(Token()) }\n\
}\n\
fun box(): String {\n\
    define<Root, Token>(Token())\n\
    defineOrdinary(\"plain\")\n\
    if (result() != \"OK\") return \"Fail top-level\"\n\
    ConsumerHost().install()\n\
    if (result() != \"OK\") return \"Fail member\"\n\
    ConsumerHost().installCarrier()\n\
    return result()\n\
}\n";
    let jdk = common::jdk_modules();
    let classpath = [library.clone(), common::stdlib_jar(), jdk.clone()];
    let reference =
        common::kotlinc_box_result_with_classpath(source, std::slice::from_ref(&library));
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    assert_eq!(
        common::compile_and_run_box(source, "ExternalMixedReified", &classpath, Some(&jdk)),
        Some(reference)
    );
    let _ = std::fs::remove_dir_all(library);
}

#[test]
fn a_dependency_member_extension_keeps_its_value_receiver_and_reified_lambda() {
    let library_source = "\
package external_reified_value\n\
interface Item\n\
interface Selected : Item\n\
object ReceiverItem : Selected\n\
object CandidateItem : Selected\n\
object StoredItem : Item\n\
object OtherItem : Item\n\
@JvmInline value class Carrier(val item: Item)\n\
var check: (Item) -> Item? = { null }\n\
open class Host(val enabled: Boolean) {\n\
    inline fun <T : Item, reified R : Item> Carrier.install(value: T) {\n\
        val receiver = item\n\
        val stored = object { val value: T = value }\n\
        check = { candidate ->\n\
            if (enabled && receiver is R && candidate is R) stored.value else null\n\
        }\n\
    }\n\
}\n\
fun installedCorrectly(): Boolean =\n\
    check(CandidateItem) === StoredItem && check(OtherItem) == null\n";
    let source = "\
import external_reified_value.*\n\
class ConsumerHost : Host(true) {\n\
    fun install() { Carrier(ReceiverItem).install<Item, Selected>(StoredItem) }\n\
}\n\
fun box(): String {\n\
    ConsumerHost().install()\n\
    return if (installedCorrectly()) \"OK\" else \"Fail\"\n\
}\n";
    let Some(library) = common::kotlinc_library(library_source) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let reference =
        common::kotlinc_box_result_with_classpath(source, std::slice::from_ref(&library));
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classpath = [library.clone(), stdlib];
    assert_eq!(
        common::expect_box_run(
            source,
            "ExternalReifiedValueMemberExtension",
            &classpath,
            Some(jdk.as_path()),
        ),
        reference,
        "krusty and kotlinc box results differ",
    );
    let _ = std::fs::remove_dir_all(library);
}
