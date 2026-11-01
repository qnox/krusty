//! kotlinc boxes the operand type of a written `is` or `as` without its type mapper, so a nested
//! class only such an operator names gets no `InnerClasses` row. A compiler-inserted cast (the
//! narrowing after an `as?`'s test), a local variable of the type, a constructor call and a member
//! call all map it, and each of those lists the row.
use super::common;

#[test]
fn a_written_type_operator_alone_lists_no_nested_class() {
    common::assert_classes_identical_to_kotlinc(
        "TypeOperandRows",
        r#"
package rows

class Outer {
    class Tested
    class NotTested
    class Cast
    class NullableCast
    class SafeCast
    class Stored
    class Created
    class Called { fun m() = 1 }
}

class Test { fun f(o: Any) = o is Outer.Tested }
class NotTest { fun f(o: Any) = o !is Outer.NotTested }
class Cast { fun f(o: Any): Any = o as Outer.Cast }
class NullableCast { fun f(o: Any?): Any? = o as Outer.NullableCast? }
class SafeCast { fun f(o: Any) = (o as? Outer.SafeCast) != null }
class Stored { fun f(o: Any): Any { val stored = o as Outer.Stored; return stored } }
class Created { fun f(): Any = Outer.Created() }
class Called { fun f(o: Any) = if (o is Outer.Called) o.m() else 0 }
"#,
        &[
            "rows/Test",
            "rows/NotTest",
            "rows/Cast",
            "rows/NullableCast",
            "rows/SafeCast",
            "rows/Stored",
            "rows/Created",
            "rows/Called",
        ],
    );
}
