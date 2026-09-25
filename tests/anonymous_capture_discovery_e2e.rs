//! An anonymous object's captures are found where the object is constructed, by the same check
//! that then types its body with them. These cases reach one construction more than once, or
//! before the object's type arguments are inferred, and each must keep every capture the object's
//! body uses, with its final type.
use super::common::{self, compare_with_kotlinc_plugin};

/// The type of every field of `class`, in classfile order: its descriptor and, when it has one,
/// its generic signature. Capture fields are named differently by kotlinc; their types are not.
fn field_types(disassembly: &str) -> Vec<String> {
    let lines: Vec<&str> = disassembly.lines().map(str::trim).collect();
    let mut fields = Vec::new();
    for (index, pair) in lines.windows(2).enumerate() {
        let is_field = pair[0].ends_with(';') && !pair[0].contains('(') && !pair[0].contains('=');
        let Some(descriptor) = pair[1].strip_prefix("descriptor: ") else {
            continue;
        };
        if !is_field {
            continue;
        }
        let signature = lines[index + 2..]
            .iter()
            .take_while(|line| !line.is_empty())
            .find_map(|line| line.strip_prefix("Signature: "))
            .and_then(|signature| signature.rsplit("// ").next())
            .unwrap_or("");
        fields.push(format!("{descriptor} {signature}").trim_end().to_string());
    }
    fields
}

fn assert_field_types_match_kotlinc(stem: &str, src: &str, class: &str) {
    let built = compare_with_kotlinc_plugin(stem, src, class, &[common::stdlib_jar()], "25", &[])
        .expect("reference kotlinc is provisioned");
    let reference = field_types(&built.reference);
    assert!(
        !reference.is_empty(),
        "{class}: kotlinc emits capture fields"
    );
    assert_eq!(field_types(&built.krusty), reference, "{class} field types");
}

const LOCAL_CLASS_PROPERTY: &str = "class Outer(val outerProp: String) {\n\
     \x20   fun foo(arg: String): String {\n\
     \x20       class Local {\n\
     \x20           val obj = object {\n\
     \x20               override fun toString() = outerProp + arg\n\
     \x20           }\n\
     \x20       }\n\
     \x20       return Local().obj.toString()\n\
     \x20   }\n\
     }\n\
     fun box(): String = Outer(\"O\").foo(\"K\")\n";

/// The object in a local class's property initializer is reached from the initializer and again
/// from the constructor. The second visit must keep the outer instance the first found, although
/// the object's method is not checked again.
#[test]
fn an_object_in_a_local_class_property_keeps_the_outer_instance() {
    common::expect_box_ok_with_stdlib(LOCAL_CLASS_PROPERTY, "LocalClassPropertyObject");
    assert_field_types_match_kotlinc(
        "LocalClassPropertyObjectFields",
        LOCAL_CLASS_PROPERTY,
        "Outer$foo$Local$obj$1",
    );
}

/// Each captured parameter of a companion member is read from its own field: `next` from the
/// captured function, never from the captured `nil`.
#[test]
fn a_companion_object_reads_each_capture_from_its_own_field() {
    common::expect_box_ok_with_stdlib(
        "interface Nat<T> {\n\
         \x20   val nil: T\n\
         \x20   fun T.next(): T\n\
         \x20   companion object {\n\
         \x20       operator fun <T> invoke(nil: T, next: T.() -> T): Nat<T> =\n\
         \x20           object : Nat<T> {\n\
         \x20               override val nil: T = nil\n\
         \x20               override fun T.next(): T = next()\n\
         \x20           }\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val count = Nat(0) { this + 1 }\n\
         \x20   val two = with(count) { nil.next().next() }\n\
         \x20   return if (two == 2) \"OK\" else \"fail: $two\"\n\
         }\n",
        "CompanionObjectCaptures",
    );
}

const BUILDER_OBJECTS: &str = "fun strings() {\n\
     \x20   buildList {\n\
     \x20       object {\n\
     \x20           fun foo() = add(\"\")\n\
     \x20       }\n\
     \x20   }\n\
     }\n\
     fun ints() {\n\
     \x20   buildList {\n\
     \x20       object {\n\
     \x20           var x: Int\n\
     \x20               get() = 1\n\
     \x20               set(value) {\n\
     \x20                   add(value)\n\
     \x20               }\n\
     \x20       }\n\
     \x20   }\n\
     }\n\
     fun box(): String {\n\
     \x20   strings()\n\
     \x20   ints()\n\
     \x20   return \"OK\"\n\
     }\n";

/// A builder lambda's object is reached before and after the builder's element type is inferred.
/// Its captured receiver has the inferred type, not the builder's type parameter.
#[test]
fn an_object_in_a_builder_lambda_captures_the_inferred_receiver() {
    common::expect_box_ok_with_stdlib(BUILDER_OBJECTS, "BuilderLambdaObjects");
    assert_field_types_match_kotlinc(
        "BuilderLambdaObjectStrings",
        BUILDER_OBJECTS,
        "BuilderLambdaObjectStringsKt$strings$1$1",
    );
    assert_field_types_match_kotlinc(
        "BuilderLambdaObjectInts",
        BUILDER_OBJECTS,
        "BuilderLambdaObjectIntsKt$ints$1$1",
    );
}
