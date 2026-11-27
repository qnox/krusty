//! An uninitialized `lateinit` read leaves the throw's coerced `Unit` result for the join or the
//! getter's return, as kotlinc does: `Unit.INSTANCE` for a property stored as `Object` (`Any`, an
//! erased type parameter) and `null` for a narrower reference. Covers top-level, private
//! top-level, member and private member default getters, the raw private reads after
//! `access$get<X>$p` and `getfield`, and a `String`/`CharSequence` control. Each class is compared
//! byte for byte with kotlinc's.

use super::common;

const SLOTS: &str = "lateinit var any: Any
private lateinit var hidden: Any
lateinit var text: String

class Slots<T : Any> {
    lateinit var member: Any
    private lateinit var secret: Any
    lateinit var generic: T
    lateinit var chars: CharSequence

    fun read() = secret.toString() + hidden.toString()
}
";

#[test]
fn object_typed_lateinit_guards_leave_unit_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "LateinitObjectSlots",
        SLOTS,
        &["LateinitObjectSlotsKt", "Slots"],
    );
}
