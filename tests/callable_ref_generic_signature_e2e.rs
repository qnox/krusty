//! A generic function reference is reflected by the declaration's erased JVM signature.
//!
//! `::foo` typed as `KFunction1<Int, Int>` still names `foo(Ljava/lang/Object;)Ljava/lang/Object;`,
//! so kotlin-reflect reports the declaration type parameter rather than the use-site `Int`.

use super::common;

fn reflect_jar() -> std::path::PathBuf {
    common::dist_jar("kotlin-reflect.jar")
        .or_else(|| common::find_jar("kotlin-reflect-", &["sources"]))
        .expect("kotlin-reflect.jar from the provisioned kotlinc distribution")
}

fn agree(source: &str) {
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
fn a_specialized_generic_function_reference_reports_the_declaration_type_parameter() {
    agree(
        r#"
        fun <T> foo(x: T) = x

        fun box(): String {
            val bar: kotlin.reflect.KFunction1<Int, Int> = ::foo
            val returnType = bar.returnType.toString()
            if (returnType != "T") return returnType
            return "OK"
        }
        "#,
    );
}

#[test]
fn a_bounded_generic_function_reference_reports_the_declaration_type_parameter() {
    agree(
        r#"
        fun <T : CharSequence> foo(x: T): T = x

        fun box(): String {
            val bar: kotlin.reflect.KFunction1<String, String> = ::foo
            val returnType = bar.returnType.toString()
            if (returnType != "T") return "ret=$returnType"
            val parameter = bar.parameters.single().type.toString()
            if (parameter != "T") return "param=$parameter"
            return "OK"
        }
        "#,
    );
}

#[test]
fn a_companion_associated_extension_omits_its_static_scope_receiver() {
    agree(
        r#"
        class C
        companion fun C.answer(value: Int): String = value.toString()

        fun box(): String {
            val ref = C::answer
            val parameters = ref.parameters.joinToString(",") { it.type.toString() }
            if (parameters != "kotlin.Int") return "params=$parameters"
            if (ref.returnType.toString() != "kotlin.String") return "ret=${ref.returnType}"
            return "OK"
        }
        "#,
    );
}

#[test]
fn a_specialized_generic_member_reference_reports_the_declaration_type_parameter() {
    agree(
        r#"
        class Box<T> {
            fun foo(x: T): T = x
        }

        fun box(): String {
            val bar: kotlin.reflect.KFunction2<Box<Int>, Int, Int> = Box<Int>::foo
            val returnType = bar.returnType.toString()
            if (returnType != "T") return returnType
            return "OK"
        }
        "#,
    );
}
