//! Default property accessors: a class's backing fields are private, with synthesized `getX()`/`setX()`
//! accessors; access from outside the declaring class goes through them (`c.x`/`c.x = v`), while inside
//! the class the field is used directly. Round-tripped under `-Xverify:all`.

mod common;

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
