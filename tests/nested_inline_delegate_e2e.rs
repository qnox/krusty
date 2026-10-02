//! Nested callable scopes retain the source inline declaration that exports delegated helpers.

use super::common;

const DECLARATIONS: &str = "import kotlin.reflect.KProperty
inline operator fun String.getValue(owner: Any?, property: KProperty<*>): String = property.name + this
object NestedOwner {
    inline fun readNested(crossinline suffix: () -> String): () -> String = {
        val nestedResult by \"O\"
        nestedResult + suffix()
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
    fun nested(): String = NestedOwner.readNested { \"K\" }()
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
fn cross_file_nested_delegate_helpers_export_the_exact_declaration_abi() {
    let sources = [
        ("NestedDeclarations.kt", DECLARATIONS),
        ("NestedCallers.kt", CALLERS),
    ];
    let reference = common::kotlinc_box_files_result(&sources, "NestedCallersKt");
    assert_eq!(reference, "OK");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources)
            .expect("compile and run nested declaration-owned helpers"),
        reference,
    );
    for owner in [
        "NestedOwner",
        "NestedDeclarationsKt",
        "NestedCaller",
        "NestedCallersKt",
    ] {
        let pair = common::ModuleClassPair::compile(&sources, owner);
        let reference =
            krusty::jvm::classreader::parse_class(&pair.kotlinc).expect("read reference owner");
        let ours = krusty::jvm::classreader::parse_class(&pair.krusty).expect("read emitted owner");
        assert_eq!(
            static_boundary_methods(&ours),
            static_boundary_methods(&reference),
            "complete private/synthetic static method ABI in {owner}",
        );
        if owner == "NestedOwner" || owner == "NestedDeclarationsKt" {
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
