//! Exact source/copy closure ownership and the separate direct-inline helper export boundary.

use super::common;

const DECLARATIONS: &str = "import kotlin.reflect.KProperty
inline operator fun String.getValue(owner: Any?, property: KProperty<*>): String = property.name + this
object NestedOwner {
    inline fun <reified T> readNested(): () -> T = {
        val nestedResult by \"OK\"
        nestedResult as T
    }
    fun readLocal(): String {
        fun localReader(): String {
            val localResult by \"OK\"
            return localResult
        }
        return localReader()
    }
}
inline val inlineProperty: String
    get() {
        val propertyResult by \"OK\"
        return propertyResult
    }
";

const CALLERS: &str = "object NestedCaller {
    fun nested(): String = NestedOwner.readNested<String>()()
    fun local(): String = NestedOwner.readLocal()
    fun property(): String = inlineProperty
}
fun box(): String {
    if (NestedCaller.nested() != \"nestedResultOK\") return \"nested delegate failed\"
    if (NestedCaller.local() != \"localResultOK\") return \"local delegate failed\"
    if (NestedCaller.property() != \"propertyResultOK\") return \"inline accessor delegate failed\"
    return \"OK\"
}
";

fn static_boundary_methods(
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

#[test]
fn same_file_nested_lambda_local_function_and_inline_accessor_delegates_execute() {
    common::expect_box_same_as_kotlinc(
        &format!("{DECLARATIONS}{CALLERS}"),
        "NestedInlineDelegates",
    );
}

#[test]
fn cross_file_nested_delegates_match_private_closure_and_direct_export_abi() {
    let sources = [
        ("NestedDeclarations.kt", DECLARATIONS),
        ("NestedCallers.kt", CALLERS),
    ];
    let reference = common::kotlinc_box_files_result(&sources, "NestedCallersKt");
    assert_eq!(reference, "OK");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("compile and run nested closure-owned helpers"),
        reference,
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    let inventory = classes
        .reference
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        classes
            .krusty
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        inventory
    );
    assert_eq!(
        inventory,
        vec![
            "NestedCaller",
            "NestedCaller$nested$$inlined$readNested$1",
            "NestedCallersKt",
            "NestedDeclarationsKt",
            "NestedOwner",
            "NestedOwner$readNested$1",
        ]
    );
    for owner in inventory {
        let reference = krusty::jvm::classreader::parse_class(&classes.reference[owner])
            .expect("read reference owner");
        let ours = krusty::jvm::classreader::parse_class(&classes.krusty[owner])
            .expect("read emitted owner");
        assert_eq!(
            static_boundary_methods(&ours),
            static_boundary_methods(&reference),
            "complete private/synthetic static method ABI in {owner}",
        );
        assert_eq!(
            static_fields(&ours),
            static_fields(&reference),
            "complete static field ABI in {owner}"
        );
        if owner == "NestedOwner$readNested$1"
            || owner == "NestedCaller$nested$$inlined$readNested$1"
        {
            assert_eq!(reference.access, 0x0031);
            assert_eq!(ours.access, reference.access);
            assert_eq!(
                static_boundary_methods(&reference),
                vec![(
                    "invoke$lambda-0".to_owned(),
                    "(Ljava/lang/String;)Ljava/lang/String;".to_owned(),
                    0x001a,
                )]
            );
            assert_eq!(
                complete_method_abi(&ours),
                complete_method_abi(&reference),
                "complete closure ABI, including erased invoke and generic signature, in {owner}"
            );
        }
        if owner == "NestedDeclarationsKt" {
            use krusty::jvm::classreader::{ACC_PUBLIC, ACC_STATIC, ACC_SYNTHETIC};
            let exported = reference
                .methods
                .iter()
                .filter(|method| {
                    method.access & (ACC_PUBLIC | ACC_STATIC | ACC_SYNTHETIC)
                        == ACC_PUBLIC | ACC_STATIC | ACC_SYNTHETIC
                })
                .map(|method| {
                    (
                        method.name.clone(),
                        method.descriptor.clone(),
                        method.access,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(exported.len(), 1, "one exported helper boundary in {owner}");
            assert_eq!(
                exported[0].1, "(Ljava/lang/String;)Ljava/lang/String;",
                "declaration-erased exported helper signature in {owner}",
            );
            assert_eq!(exported[0].2, 0x1019);
        }
    }
}

#[test]
fn retained_nested_delegate_matches_the_owned_kt42253_class_inventory_and_helper_abi() {
    let sources = [
        ("Library.kt", "import kotlin.reflect.KProperty
inline operator fun String.getValue(owner: Any?, property: KProperty<*>): String = property.name + this
object C {
    inline fun inlineFun() = {
        val O by \"K\"
        O
    }.let { it() }
}"),
        ("Main.kt", "object ForceOutOfOrder { fun callInline() = C.inlineFun() }
fun box(): String = if (ForceOutOfOrder.callInline() == \"OK\") \"OK\" else \"Fail\""),
    ];
    assert_eq!(common::kotlinc_box_files_result(&sources, "MainKt"), "OK");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("retained source closure executes"),
        "OK"
    );
    let classes = common::classes_against_kotlinc_module(&sources);
    let inventory = classes
        .reference
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        inventory,
        vec![
            "C",
            "C$inlineFun$1",
            "ForceOutOfOrder",
            "LibraryKt",
            "MainKt"
        ]
    );
    assert_eq!(
        classes
            .krusty
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        inventory
    );
    for owner in inventory {
        let reference = krusty::jvm::classreader::parse_class(&classes.reference[owner])
            .expect("reference class");
        let ours =
            krusty::jvm::classreader::parse_class(&classes.krusty[owner]).expect("emitted class");
        assert_eq!(
            static_boundary_methods(&ours),
            static_boundary_methods(&reference),
            "complete private/synthetic static ABI in {owner}"
        );
        assert_eq!(
            static_fields(&ours),
            static_fields(&reference),
            "complete static field ABI in {owner}"
        );
        if owner == "C$inlineFun$1" {
            assert_eq!(
                static_boundary_methods(&reference),
                vec![(
                    "invoke$lambda-0".to_owned(),
                    "(Ljava/lang/String;)Ljava/lang/String;".to_owned(),
                    0x001a,
                )]
            );
            assert_eq!(complete_method_abi(&ours), complete_method_abi(&reference));
        }
    }
}

fn static_fields(
    class: &krusty::jvm::classreader::ClassInfo,
) -> Vec<(String, String, u16, Option<String>)> {
    class
        .fields
        .iter()
        .filter(|field| field.access & krusty::jvm::classreader::ACC_STATIC != 0)
        .map(|field| {
            (
                field.name.clone(),
                field.descriptor.clone(),
                field.access,
                field.signature.clone(),
            )
        })
        .collect()
}

fn complete_method_abi(
    class: &krusty::jvm::classreader::ClassInfo,
) -> Vec<(String, String, u16, Option<String>)> {
    class
        .methods
        .iter()
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
                method.signature.clone(),
            )
        })
        .collect()
}
