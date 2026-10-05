//! When a property's `JvmFieldSignature` records the backing field's descriptor, measured against
//! kotlinc 2.4.20. kotlinc writes it exactly when a reader cannot rebuild it by mapping the
//! property type's class id (`requiresSignature`): a type parameter has no class id, and a boxed
//! nullable primitive, a value class or a reference array maps to something else. A property with
//! no backing field interns no field descriptor at all.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const DECLARATIONS: &str = "package app\n\
    \n\
    @JvmInline value class Id(val raw: String)\n\
    class Item\n\
    class Box<T>(val content: T) {\n\
    \x20   operator fun getValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>): T = content\n\
    }\n";

#[test]
fn a_class_records_the_field_descriptors_a_reader_cannot_derive() {
    let src = format!(
        "{DECLARATIONS}\n\
         class Holder<T>(init: T, val names: Array<String>, val numbers: IntArray, val id: Id) {{\n\
         \x20   private var hidden: T = init\n\
         \x20   var shown: T = init\n\
         \x20   private val count: Int? = null\n\
         \x20   private var items: Array<Item>? = null\n\
         \x20   private val plain: Item = Item()\n\
         \x20   val delegated: Item by Box(Item())\n\
         \x20   val same: Box<Item> by Box(Box(Item()))\n\
         \x20   fun touch() = hidden.toString() + count + items + plain\n\
         }}\n"
    );
    assert_identical("FieldSignatureClass", &src, "app/Holder");
}

#[test]
fn an_abstract_property_interns_no_field_descriptor() {
    let src = format!(
        "{DECLARATIONS}\n\
         interface Source<T> {{\n\
         \x20   var current: T\n\
         \x20   val count: Int?\n\
         }}\n\
         abstract class Base<T> {{\n\
         \x20   abstract var current: T\n\
         \x20   abstract val names: Array<String>\n\
         }}\n"
    );
    assert_identical("FieldSignatureInterface", &src, "app/Source");
    assert_identical("FieldSignatureInterface", &src, "app/Base");
}

#[test]
fn a_file_facade_records_the_field_descriptors_a_reader_cannot_derive() {
    let src = format!(
        "{DECLARATIONS}\n\
         val unsigned: UInt = 1u\n\
         val wide: ULong = 2uL\n\
         var count: Int? = null\n\
         lateinit var names: Array<String>\n\
         var numbers: IntArray? = null\n\
         private var hidden: Id? = null\n\
         val delegated: Item by Box(Item())\n\
         val same: Box<Item> by Box(Box(Item()))\n\
         fun touch() = hidden.toString()\n"
    );
    assert_identical("FieldSignatureFacade", &src, "app/FieldSignatureFacadeKt");
}

#[test]
fn a_companion_property_records_its_hoisted_field_descriptor() {
    let src = format!(
        "{DECLARATIONS}\n\
         class Owner {{\n\
         \x20   companion object {{\n\
         \x20       var count: Int? = null\n\
         \x20       val names: Array<String>? = null\n\
         \x20       val plain: Item = Item()\n\
         \x20       val id: Id = Id(\"\")\n\
         \x20   }}\n\
         }}\n"
    );
    assert_identical("FieldSignatureCompanion", &src, "app/Owner$Companion");
}
