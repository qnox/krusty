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
        package test

        interface H<T> {
            fun foo(): T?
        }

        interface A : H<A>

        fun box(): String {
            val onSubtype = A::foo.returnType.toString()
            if (onSubtype != "test.A?") return onSubtype
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
