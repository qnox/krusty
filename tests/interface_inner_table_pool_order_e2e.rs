//! PLATFORM: JVM compatibility holders share ordinary InnerClasses constant-pool ordering.

use super::common;

#[test]
fn annotation_only_nested_references_precede_the_compatibility_holder() {
    let source = r#"import kotlin.reflect.KClass
annotation class Mark(val kind: KClass<*>)
class Arguments { class Token }
interface Contract {
    @Mark(Arguments.Token::class)
    fun value(): String = "OK"
}
"#;
    let classes = common::classes_against_kotlinc_module(&[("Before.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn annotation_only_nested_references_follow_the_compatibility_holder() {
    let source = r#"import kotlin.reflect.KClass
annotation class Mark(val kind: KClass<*>)
class Holder { class Token }
interface Contract {
    @Mark(Holder.Token::class)
    fun value(): String = "OK"
}
"#;
    let classes = common::classes_against_kotlinc_module(&[("Annotated.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn declared_children_and_the_compatibility_holder_follow_one_table_order() {
    let source = r#"interface Contract {
    class Alpha
    interface Zeta
    fun value(): String = "OK"
}
"#;
    let classes = common::classes_against_kotlinc_module(&[("Declared.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_abstract_interface_keeps_its_children_without_a_compatibility_holder() {
    let source = r#"interface Contract {
    class Alpha
    interface Zeta
    fun value(): String
}
"#;
    let classes = common::classes_against_kotlinc_module(&[("Abstract.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
