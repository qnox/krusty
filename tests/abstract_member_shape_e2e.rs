//! An abstract property has no backing field, so kotlinc writes no field entry in its
//! `JvmPropertySignature` and interns none of its strings: `interface Holder { val count: Int? }`
//! records `getCount()Ljava/lang/Integer;` and no separate `Ljava/lang/Integer;` field descriptor.
//! An abstract member of an abstract class carries the same nullability annotations an interface
//! member does.

use super::common;

const DECLARATIONS: &str = "interface Holder { val count: Int?; var total: Long? }
interface Slot<T> { val value: T; var next: T }
abstract class Gauge { abstract val level: Int?; abstract var limit: Long? }
abstract class Cell<T> { abstract val content: T }
abstract class Shape {
    abstract fun area(unit: String): String?
    abstract val label: String
}
";

#[test]
fn abstract_members_match_kotlinc() {
    let sources = [("Declarations.kt", DECLARATIONS)];
    for class in ["Holder", "Slot", "Gauge", "Cell", "Shape"] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        assert!(pair.krusty == pair.kotlinc, "{class} differs from kotlinc");
    }
}
