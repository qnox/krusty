//! Escaping reified lambdas that cross a local delegated-property accessor boundary. Kotlin keeps
//! the selected delegate convention as one erased declaration helper even when that convention is
//! itself inline and reified; call-site specialization belongs to the surrounding expression and
//! lambda copies, not to cloned delegate plans.

use super::common;

#[test]
fn a_generic_receiver_delegate_keeps_its_own_parameter_scope() {
    common::expect_box_same_as_kotlinc(
        "class Unrelated<T>\n\
         open class Storage<T>(val value: T) {\n\
             operator fun getValue(owner: Any?, property: Any?): T = value\n\
         }\n\
         class Container<R>(value: R) : Storage<R>(value)\n\
         fun read(value: String): String {\n\
             val local by Container(value)\n\
             return local\n\
         }\n\
         fun box(): String = read(\"OK\")\n",
        "GenericReceiverDelegateScope",
    );
}

const REUSED_GENERIC_PLAN: &str = "import kotlin.reflect.KProperty
interface Root
class Token : Root
class Carrier<T : Root>(private val value: T) {
    operator fun getValue(owner: Any?, property: KProperty<*>): T = value
}
inline fun <reified T : Root> delegated(value: T): T {
    val local by Carrier(value)
    return local
}
fun exact(value: Token): Root = delegated<Token>(value)
fun erased(value: Root): Root = delegated<Root>(value)
object DelegateHost {
    fun erased(value: Root): Root = delegated<Root>(value)
}
fun box(): String {
    val token = Token()
    return if (
        exact(token) === token &&
        erased(token) === token &&
        DelegateHost.erased(token) === token
    ) \"OK\" else \"Fail\"
}
";

const REUSED_REIFIED_PLAN: &str = "import kotlin.reflect.KProperty
interface Item
class Token : Item
class Root : Item
class Delegate<T>(val value: Any?)
inline operator fun <reified T> Delegate<T>.getValue(
    owner: Any?,
    property: KProperty<*>,
): T = value as T
inline fun <reified T> install(value: Any?): () -> T {
    val local by Delegate<T>(value)
    return { local }
}
fun box(): String {
    val token = install<Token>(Token())
    val root = install<Root>(Root())
    return if (token() is Token && root() is Root) \"OK\" else \"Fail\"
}
";

#[test]
fn an_escaping_lambda_specializes_a_cast_after_reading_a_local_delegate() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate(private val value: Any?) {\n\
             operator fun getValue(owner: Any?, property: Any?): Any? = value\n\
         }\n\
         var read: () -> Any? = { null }\n\
         inline fun <reified R : Item> install(value: Any?) {\n\
             val local by Delegate(value)\n\
             read = { local as? R }\n\
         }\n\
         fun box(): String {\n\
             install<Token>(Token())\n\
             val kept = read()\n\
             install<Token>(Root())\n\
             return if (kept is Token && read() == null) \"OK\" else \"Fail\"\n\
         }\n",
        "EscapingReifiedLocalDelegate",
    );
}

#[test]
fn an_omitted_noinline_default_specializes_after_reading_a_local_delegate() {
    common::expect_box_same_as_kotlinc(
        "interface Item\n\
         class Token : Item\n\
         class Root : Item\n\
         class Delegate(private val value: Any?) {\n\
             operator fun getValue(owner: Any?, property: Any?): Any? = value\n\
         }\n\
         inline fun <reified R : Item> read(\n\
             value: Any?,\n\
             noinline action: () -> Any? = {\n\
                 val local by Delegate(value)\n\
                 local as? R\n\
             }\n\
         ): Any? = action()\n\
         fun box(): String {\n\
             val kept = read<Token>(Token())\n\
             val rejected = read<Token>(Root())\n\
             return if (kept is Token && rejected == null) \"OK\" else \"Fail\"\n\
         }\n",
        "ReifiedLocalDelegateDefaultLambda",
    );
}

#[test]
fn generic_inline_copies_reuse_one_erased_declaration_helper() {
    common::expect_box_same_as_kotlinc(REUSED_GENERIC_PLAN, "ReusedGenericDelegatePlan");

    let owners = ["ReusedGenericDelegatePlanKt", "DelegateHost"];
    let classes = common::compile_with_kotlinc(
        "ReusedGenericDelegatePlan",
        REUSED_GENERIC_PLAN,
        &[],
        &owners,
    );
    for (owner, (reference, ours)) in owners.into_iter().zip(classes) {
        let reference =
            krusty::jvm::classreader::parse_class(&reference).expect("read kotlinc-emitted owner");
        let ours = krusty::jvm::classreader::parse_class(&ours).expect("read krusty-emitted owner");
        let reference_helpers = private_and_synthetic_static_methods(&reference);
        let expected = if owner == "ReusedGenericDelegatePlanKt" {
            vec![
                ("delegated".to_owned(), "(LRoot;)LRoot;".to_owned(), 0x1019),
                (
                    "delegated$lambda-0".to_owned(),
                    "(LCarrier;)LRoot;".to_owned(),
                    0x101a,
                ),
                (
                    "access$delegated$lambda-0".to_owned(),
                    "(LCarrier;)LRoot;".to_owned(),
                    0x1019,
                ),
            ]
        } else {
            Vec::new()
        };
        assert_eq!(
            reference_helpers, expected,
            "the reference retains one declaration helper and its access boundary in {owner}",
        );
        assert_eq!(
            private_and_synthetic_static_methods(&ours),
            reference_helpers,
            "inline copies must retain the declaration's helper and accessor ABI in {owner}",
        );
    }
}

#[test]
fn reified_delegate_accessor_plan_stays_one_erased_declaration_helper() {
    // The reused helper keeps the declaration's reified marker, so invoking the lambda throws.
    // Both compilers do that; a successful `OK` would mean the helper was specialized.
    let reference = common::kotlinc_box_result(REUSED_REIFIED_PLAN);
    let krusty =
        common::expect_box_run_with_stdlib(REUSED_REIFIED_PLAN, "ReusedReifiedDelegatePlan");
    assert_eq!(krusty, reference, "ReusedReifiedDelegatePlan");
    assert!(
        reference.starts_with("ERROR:UnsupportedOperationException:"),
        "the shared helper still executes its reified marker: {reference}"
    );

    let pair = common::ModuleClassPair::compile(
        &[("ReusedReifiedDelegatePlan.kt", REUSED_REIFIED_PLAN)],
        "ReusedReifiedDelegatePlanKt",
    );
    let reference =
        krusty::jvm::classreader::parse_class(&pair.kotlinc).expect("read kotlinc-emitted facade");
    let ours =
        krusty::jvm::classreader::parse_class(&pair.krusty).expect("read krusty-emitted facade");
    let helper_flags = krusty::jvm::classreader::ACC_PRIVATE | krusty::jvm::classreader::ACC_STATIC;
    let helper_descriptors = |class: &krusty::jvm::classreader::ClassInfo, suffix: &str| {
        class
            .methods
            .iter()
            .filter(|method| {
                method.access & helper_flags == helper_flags
                    && method.name.ends_with(suffix)
                    && method.descriptor == "(LDelegate;)Ljava/lang/Object;"
            })
            .map(|method| method.descriptor.clone())
            .collect::<Vec<_>>()
    };
    let reference_helpers = helper_descriptors(&reference, "$lambda-0");
    assert_eq!(
        reference_helpers,
        vec!["(LDelegate;)Ljava/lang/Object;".to_owned()],
        "the reference plan is one declaration-erased helper",
    );
    assert_eq!(
        helper_descriptors(&ours, "$lambda-0"),
        reference_helpers,
        "Token and Root lambda copies must share the declaration helper",
    );
}

fn private_and_synthetic_static_methods(
    class: &krusty::jvm::classreader::ClassInfo,
) -> Vec<(String, String, u16)> {
    use krusty::jvm::classreader::{ACC_PRIVATE, ACC_STATIC, ACC_SYNTHETIC};
    class
        .methods
        .iter()
        .filter(|method| {
            method.access & ACC_STATIC != 0 && method.access & (ACC_PRIVATE | ACC_SYNTHETIC) != 0
        })
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect()
}

const REFLECTION_DECLARATION: &str = "import kotlin.reflect.KProperty
interface Item
class Token : Item
var firstProperty: KProperty<*>? = null
var identityChanged = false
class Parcel<T : Item>(private val value: T) {
    operator fun getValue(owner: Any?, property: KProperty<*>): T {
        val previous = firstProperty
        if (previous == null) firstProperty = property
        else if (previous != property) identityChanged = true
        return value
    }
}
inline fun <reified T : Item> unpack(value: T): T {
    val payload by Parcel(value)
    return payload
}
";

const REFLECTION_CALLER: &str = "object ReaderHost {
    fun read(value: Item): Item = unpack<Item>(value)
}
fun box(): String {
    val token = Token()
    if (unpack<Token>(token) !== token) return \"facade read failed\"
    if (ReaderHost.read(token) !== token) return \"host read failed\"
    return if (identityChanged) \"reflection identity changed\" else \"OK\"
}
";

#[test]
fn inline_delegate_reflection_identity_stays_with_its_declaration() {
    let source = format!("{REFLECTION_DECLARATION}{REFLECTION_CALLER}");
    common::expect_box_same_as_kotlinc(&source, "DeclarationDelegateReflection");
}

#[test]
fn cross_file_inline_delegate_reflection_and_helper_abi_stay_with_the_declaration() {
    let sources = [
        ("ReflectionDeclaration.kt", REFLECTION_DECLARATION),
        ("ReflectionCaller.kt", REFLECTION_CALLER),
    ];
    let reference = common::kotlinc_box_files_result(&sources, "ReflectionCallerKt");
    assert_eq!(reference, "OK", "the reference retains reflection identity");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("compile and run the cross-file declaration-owned delegate"),
        reference,
    );
    for owner in [
        "ReflectionDeclarationKt",
        "ReflectionCallerKt",
        "ReaderHost",
    ] {
        let pair = common::ModuleClassPair::compile(&sources, owner);
        let reference = krusty::jvm::classreader::parse_class(&pair.kotlinc)
            .expect("read the reference declaration/helper owner");
        let ours = krusty::jvm::classreader::parse_class(&pair.krusty)
            .expect("read the emitted declaration/helper owner");
        let reference_helpers = private_and_synthetic_static_methods(&reference);
        let expected = if owner == "ReflectionDeclarationKt" {
            vec![
                ("unpack".to_owned(), "(LItem;)LItem;".to_owned(), 0x1019),
                (
                    "unpack$lambda-0".to_owned(),
                    "(LParcel;)LItem;".to_owned(),
                    0x101a,
                ),
                (
                    "access$unpack$lambda-0".to_owned(),
                    "(LParcel;)LItem;".to_owned(),
                    0x1019,
                ),
            ]
        } else {
            Vec::new()
        };
        assert_eq!(
            reference_helpers, expected,
            "reference helper ABI in {owner}"
        );
        assert_eq!(
            private_and_synthetic_static_methods(&ours),
            reference_helpers,
            "a foreign inline copy must call, not redeclare, the helper in {owner}",
        );
    }
}

#[test]
fn foreign_and_current_delegate_helpers_with_equal_spellings_keep_distinct_owners() {
    const CALLER: &str = "package caller
import Item
import Token
import Parcel
import firstProperty
import identityChanged
import unpack as declarationRead
inline fun <reified T : Item> unpack(value: T): T {
    val payload by Parcel(value)
    return payload
}
fun box(): String {
    val token = Token()
    declarationRead<Token>(token)
    if (unpack<Item>(token) !== token) return \"current read failed\"
    if (!identityChanged) return \"distinct declarations shared reflection identity\"
    firstProperty = null
    identityChanged = false
    declarationRead<Item>(token)
    declarationRead<Token>(token)
    return if (identityChanged) \"foreign declaration changed identity\" else \"OK\"
}
";
    let sources = [
        ("ReflectionDeclaration.kt", REFLECTION_DECLARATION),
        ("CollidingHelpers.kt", CALLER),
    ];
    let reference = common::kotlinc_box_files_result(&sources, "caller.CollidingHelpersKt");
    assert_eq!(
        reference, "OK",
        "the reference separates declaration owners"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("compile and run equally spelled declaration-owned helpers"),
        reference,
    );
}
