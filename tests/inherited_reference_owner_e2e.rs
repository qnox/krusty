//! An unbound reference to an inherited member is owned by the classifier it was written on.
//!
//! `A::foo` reflects `A`, so kotlin-reflect substitutes `T?` to `A?`. `H<A>::foo` still reflects
//! `H`, and the return type stays the declaration parameter.

use super::common;

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

#[test]
fn an_unbound_inherited_member_reference_substitutes_through_the_referenced_classifier() {
    let source = r#"
        interface H<T> {
            fun foo(): T?
        }

        interface A : H<A>

        fun box(): String {
            val onSubtype = A::foo.returnType.toString()
            if (onSubtype != "A?") return onSubtype
            val onDeclaration = H<A>::foo.returnType.toString()
            if (onDeclaration != "T?") return onDeclaration
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

/// An unbound extension is not an inherited instance member. Its reflection owner stays the file
/// facade even though the function type's first parameter is a different classifier.
#[test]
fn an_unbound_extension_reference_keeps_the_file_facade_owner() {
    let source = r#"
        import kotlin.reflect.jvm.javaMethod

        class Extension(val name: String)
        class Dispatch {
            fun read(): String = "dispatch"
        }
        fun Extension.member(): String = name

        fun box(): String {
            val extension = Extension::member
            val declared = extension.javaMethod?.declaringClass?.name ?: return "no-method"
            if (!declared.endsWith("Kt")) return declared
            val parameters = extension.parameters.joinToString(",") { "${it.kind}:${it.type}" }
            if (parameters != "EXTENSION_RECEIVER:Extension") return parameters
            return if (extension(Extension("OK")) == "OK") "OK" else "call"
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

/// A member extension is both a member and an extension. Callable references to it are prohibited,
/// so reflection must not retarget the dispatch class onto the extension receiver.
#[test]
fn a_member_extension_reference_is_prohibited() {
    let source = r#"
        class Extension(val name: String)
        class Dispatch {
            fun Extension.member(): String = name
            fun ref() = Extension::member
        }
    "#;
    common::assert_messages_match_kotlinc(source);
}
