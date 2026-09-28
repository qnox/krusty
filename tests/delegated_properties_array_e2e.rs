//! kotlinc keeps the `KProperty` references its delegated-property operators receive in one
//! `$$delegatedProperties` array per class (`PropertyReferenceLowering`): a leading
//! `static final synthetic` field, filled first in `<clinit>` in declaration order, read as
//! `$$delegatedProperties[i]`. Such a `<clinit>` has no line table, and a delegate's constructor
//! store maps to its property's line.
//!
//! Each case asserts that the named classes are byte-identical to kotlinc's. The fixtures use
//! neutral names only.
use super::common;

fn assert_identical(stem: &str, src: &str, classes: &[&str]) {
    common::assert_classes_identical_to_kotlinc(stem, src, classes);
}

const CELL: &str = "import kotlin.reflect.KProperty\n\
     \n\
     class Cell(val stored: Int) {\n\
     \x20   operator fun getValue(owner: Any?, property: KProperty<*>): Int = stored\n\
     \x20   operator fun setValue(owner: Any?, property: KProperty<*>, value: Int) {}\n\
     }\n\
     \n\
     fun touch(value: Int): Int = value\n";

#[test]
fn a_class_keeps_its_delegated_property_references_in_one_array() {
    let src = format!(
        "{CELL}\n\
         class Shelf {{\n\
         \x20   val left: Int by Cell(1)\n\
         \x20   var right: Int by Cell(2)\n\
         \x20   val plain: Int = touch(3)\n\
         }}\n\
         \n\
         class Crate {{\n\
         \x20   val inner: Int by Cell(4)\n\
         \x20   companion object {{\n\
         \x20       val shared: Int = touch(5)\n\
         \x20   }}\n\
         }}\n"
    );
    assert_identical("ShelfArray", &src, &["Shelf", "Crate", "Crate$Companion"]);
}

#[test]
fn an_object_fills_its_array_before_its_instance() {
    let src = format!(
        "{CELL}\n\
         object Rack {{\n\
         \x20   val first: Int = touch(1)\n\
         \x20   val slot: Int by Cell(2)\n\
         \x20   var spare: Int by Cell(3)\n\
         \x20   init {{\n\
         \x20       touch(4)\n\
         \x20   }}\n\
         }}\n"
    );
    assert_identical("RackArray", &src, &["Rack"]);
}

#[test]
fn an_enum_fills_its_array_before_its_entries() {
    let src = format!(
        "{CELL}\n\
         enum class Tier {{\n\
         \x20   LOW, HIGH;\n\
         \x20   val weight: Int by Cell(1)\n\
         }}\n"
    );
    assert_identical("TierArray", &src, &["Tier"]);
}
