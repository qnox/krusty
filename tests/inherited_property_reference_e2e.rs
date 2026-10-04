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
