//! Default property accessors: a class's backing fields are private, with synthesized `getX()`/`setX()`
//! accessors; access from outside the declaring class goes through them (`c.x`/`c.x = v`), while inside
//! the class the field is used directly. Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn property_accessors_run() {
    let src = "class Box(val x: Int, var y: String) {\n\
    fun internal(): Int = x\n\
}\n\
fun box(): String {\n\
val b = Box(10, \"a\")\n\
if (b.x != 10) return \"f1\"\n\
if (b.internal() != 10) return \"f2\"\n\
b.y = \"z\"\n\
if (b.y != \"z\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "B");
}

#[test]
fn property_setter_accepts_branching_value() {
    // A property realization that consumes a receiver must not leave it on the verifier stack while the
    // assigned value emits branch merge frames. This used to be handled by `SetField`; semantic property
    // writes need the same spill discipline whether the backend chooses a field or an accessor.
    let src = "class Holder(var value: String)\n\
var trace = \"\"\n\
val shared = Holder(\"start\")\n\
fun target(): Holder { trace += \"R\"; return shared }\n\
fun selected(): String { trace += \"V\"; return \"OK\" }\n\
fun box(): String {\n\
  val chooseFirst = true\n\
  target().value = if (chooseFirst) selected() else \"FAIL\"\n\
  return if (shared.value == \"OK\" && trace == \"RV\") \"OK\" else shared.value + trace\n\
}\n";
    common::assert_box_ok_with_stdlib(src, "BranchingProperty");
}
