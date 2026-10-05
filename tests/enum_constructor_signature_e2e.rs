//! An enum's constructor and its property fields carry kotlinc's generic `Signature` attributes,
//! spelled as an ordinary class's are (the constructor's without the synthetic name and ordinal
//! parameters). The entries read their function and property values from top-level properties, so
//! the case covers the signatures alone.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      import kotlin.reflect.KProperty1\n\
                      \n\
                      class Holder(val x: Int) {\n\
                      \x20   var y: Int = 0\n\
                      }\n\
                      \n\
                      fun twice(n: Int): Int = n * 2\n\
                      \n\
                      val doubler: (Int) -> Int = ::twice\n\
                      val readX: KProperty1<Holder, Int> = Holder::x\n\
                      val readY: KProperty1<Holder, Int> = Holder::y\n\
                      \n\
                      enum class Pick(val f: (Int) -> Int, val p: KProperty1<Holder, Int>) {\n\
                      \x20   X(doubler, readX),\n\
                      \x20   Y(doubler, readY),\n\
                      }\n";

#[test]
fn enum_constructor_signatures_are_byte_identical_to_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "EnumConstructorSignature",
        SOURCE,
        "store/Pick",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/Pick differs from kotlinc: {diff}"));
}
