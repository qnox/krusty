//! An inherited property reference is owned by the classifier it was written on.
//!
//! `A::parent` reflects `A`, so kotlin-reflect substitutes `T?` to `A?`. `H<A>::parent` still
//! reflects `H`. A class that implements the declaring interface reflects that class and reads
//! the accessor through it.

use super::common;

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

#[test]
fn common_ir_records_the_written_owner_of_each_inherited_property_reference() {
    let source = r#"
        interface H<T> {
            val parent: T?
        }

        interface A : H<A>

        fun references(a: A) {
            val unbound = A::parent
            val bound = a::parent
            val declaring = H<A>::parent
        }
    "#;
    let classpath = std::rc::Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
        common::jdk_modules(),
    ]));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let (files, diagnostics) =
        common::capture_common_ir(source, "InheritedPropertyOwner", platform);
    assert!(diagnostics.is_empty(), "frontend rejected: {diagnostics:?}");
    let owners = files
        .iter()
        .flat_map(|file| &file.exprs)
        .filter_map(|expression| match expression {
            krusty::ir::IrExpr::Checked(krusty::ir::IrCheckedOperation::PropertyReference {
                target:
                    krusty::fir::FirPropertyReferenceTarget::SpecializedModule {
                        reflection_owner, ..
                    },
                ..
            }) => reflection_owner.as_ref().map(|owner| owner.render()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(owners, ["A", "A", "H"]);
}

#[test]
fn an_inherited_property_reference_substitutes_through_the_referenced_classifier() {
    let source = r#"
        interface H<T> {
            val parent: T?
        }

        interface A : H<A>

        interface G<T> {
            val item: T?
                get() = null
        }

        class C : G<C>

        fun box(): String {
            val onSubtype = A::parent.returnType.toString()
            if (onSubtype != "A?") return onSubtype
            val onDeclaration = H<A>::parent.returnType.toString()
            if (onDeclaration != "T?") return onDeclaration
            val onClass = C::item.returnType.toString()
            if (onClass != "C?") return onClass
            if (C::item.get(C()) != null) return "get"
            return "OK"
        }
    "#;
    let reflect = reflect_jar();
    let reference =
        common::kotlinc_box_result_with_classpath(source, std::slice::from_ref(&reflect));
    assert_eq!(
        common::Fixture::new().with_reflect().run_box(source),
        reference
    );
    assert_eq!(reference, "OK");
}

#[test]
fn an_inherited_property_reference_in_another_file_uses_the_referenced_classifier() {
    let sources = [
        (
            "Types.kt",
            r#"
                interface H<T> {
                    val parent: T?
                }

                interface A : H<A>
            "#,
        ),
        (
            "Main.kt",
            r#"
                fun box(): String {
                    val onSubtype = A::parent.returnType.toString()
                    if (onSubtype != "A?") return onSubtype
                    val onDeclaration = H<A>::parent.returnType.toString()
                    if (onDeclaration != "T?") return onDeclaration
                    return "OK"
                }
            "#,
        ),
    ];
    let reflect = reflect_jar();
    let reference = common::kotlinc_box_files_result_with_classpath(
        &sources,
        "MainKt",
        std::slice::from_ref(&reflect),
    );
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty =
        common::compile_and_run_box_files(&sources, &[stdlib, reflect], Some(jdk.as_path()))
            .expect("cross-file inherited property reference");
    assert_eq!(krusty, reference);
    assert_eq!(reference, "OK");
}
