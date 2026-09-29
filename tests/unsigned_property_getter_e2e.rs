//! A member property of an unsigned type publishes the value-class-mangled accessor a property
//! reference already calls. `KProperty0<UInt>.get` invokes `getUInt-pVg5ArA()I`; a plain
//! `getUInt()I` fails at the first read with `NoSuchMethodError`.

use super::common;

const DELEGATE: &str = r#"
import kotlin.reflect.KProperty
import kotlin.reflect.KProperty0

class ByteDelegate(
    private val position: Int,
    private val uIntValue: KProperty0<UInt>
) {
    operator fun getValue(any: Any?, property: KProperty<*>): UByte {
        val uInt = uIntValue.get() shr (position * 8) and 0xffu
        return uInt.toUByte()
    }
}

class ByteDelegateTest {
    val uInt = 0xA1B2C3u
    val uByte by ByteDelegate(0, this::uInt)

    fun test() {
        val actual = uByte
        if (0xC3u.toUByte() != actual) throw AssertionError()
    }
}

fun box(): String {
    ByteDelegateTest().test()
    return "OK"
}
"#;

const MEMBERS: &str = r#"
class UnsignedMembers {
    val uInt: UInt = 1u
    var uVar: UInt = 2u
    val uByte: UByte = 3u
    val uShort: UShort = 4u
    val uLong: ULong = 5uL
    val uNull: UInt? = null
}

@JvmInline
value class UnsignedBox(val x: UInt)

fun box(): String = "OK"
"#;

fn member_table(stem: &str, source: &str, class: &str) -> (Vec<String>, Vec<String>) {
    let comparison = common::compare_with_kotlinc_plugin(
        stem,
        source,
        class,
        &[common::stdlib_jar()],
        "21",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    (
        common::member_table(&comparison.reference_bytes),
        common::member_table(&comparison.krusty_bytes),
    )
}

#[test]
fn an_unsigned_member_getter_uses_the_value_class_name() {
    let comparison = common::compare_with_kotlinc_plugin(
        "UnsignedPropertyGetter",
        DELEGATE,
        "ByteDelegateTest",
        &[common::stdlib_jar(), common::jdk_modules()],
        "21",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_eq!(
        common::member_table(&comparison.krusty_bytes),
        common::member_table(&comparison.reference_bytes),
        "ByteDelegateTest: kotlinc's member table"
    );
    let getter = "getUInt-pVg5ArA";
    assert_eq!(
        common::method_instructions(&comparison.krusty, getter),
        common::method_instructions(&comparison.reference, getter),
        "{getter}"
    );
    common::expect_box_same_as_kotlinc(DELEGATE, "UnsignedPropertyGetter");
}

#[test]
fn unsigned_member_accessors_match_kotlinc() {
    for class in ["UnsignedMembers", "UnsignedBox"] {
        let (reference, krusty) = member_table("UnsignedMemberAccessors", MEMBERS, class);
        assert_eq!(krusty, reference, "{class}: kotlinc's member table");
    }
}
