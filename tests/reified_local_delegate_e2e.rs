//! Escaping reified lambdas that cross a local delegated-property accessor boundary. Kotlin keeps
//! the selected delegate convention as one erased declaration helper even when that convention is
//! itself inline and reified; call-site specialization belongs to the surrounding expression and
//! lambda copies, not to cloned delegate plans.

use super::common;

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
fn generic_inline_copies_reuse_one_erased_plan_per_physical_owner() {
    common::expect_box_same_as_kotlinc(REUSED_GENERIC_PLAN, "ReusedGenericDelegatePlan");

    let classes =
        common::expect_classes_with_stdlib(REUSED_GENERIC_PLAN, "ReusedGenericDelegatePlan");
    for owner in ["ReusedGenericDelegatePlanKt", "DelegateHost"] {
        let bytes = classes
            .iter()
            .find_map(|(name, bytes)| (name == owner).then_some(bytes))
            .unwrap_or_else(|| panic!("missing emitted owner {owner}"));
        let ours = krusty::jvm::classreader::parse_class(bytes).expect("read krusty-emitted owner");
        let helper_flags =
            krusty::jvm::classreader::ACC_PRIVATE | krusty::jvm::classreader::ACC_STATIC;
        let helpers = ours
            .methods
            .iter()
            .filter(|method| {
                method.access & helper_flags == helper_flags
                    && method.name.ends_with("$jvm_delegate")
            })
            .map(|method| method.descriptor.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            helpers,
            vec!["(LCarrier;)LRoot;"],
            "the backend must realize one erased helper, without substitution duplicates, in {owner}",
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
        helper_descriptors(&ours, "$lambda$0"),
        reference_helpers,
        "Token and Root lambda copies must share the declaration helper",
    );
}
