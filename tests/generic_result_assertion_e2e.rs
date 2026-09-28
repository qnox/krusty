//! A `!!` over a Kotlin generic call checks the erased `Object` slot the call returns and only
//! then casts it to the substituted type, as kotlinc emits it: `dup; checkNotNull; checkcast`, not
//! `checkcast; dup; checkNotNull`. The same holds for a call to a function in another file of the
//! module and for a member of a generic class.
use super::common;

const DECLARATIONS: &str = "fun <T> keep(value: T?): T? = value\n\
    class Slot<T>(private val value: T?) { fun take(): T? = value }\n\
    class Note(val size: Int)\n";

#[test]
fn a_generic_result_is_checked_before_its_cast() {
    let source = "fun <T> same(value: T): T = value\n\
        fun local(): Note = same<Note?>(Note(1))!!\n\
        fun crossFile(): Note = keep(Note(2))!!\n\
        fun member(slot: Slot<Note>): Note = slot.take()!!\n\
        fun operand(): Int = keep(Note(3))!!.size\n";
    let pair = common::ModuleClassPair::compile(
        &[("Declarations.kt", DECLARATIONS), ("Assertions.kt", source)],
        "AssertionsKt",
    );
    for method in ["local", "crossFile", "member", "operand"] {
        let (kotlinc, krusty) = pair.method_code("AssertionsKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}
