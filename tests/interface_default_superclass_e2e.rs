//! A class forwarder for an interface default is emitted only when the class's most specific
//! declaration of that member is not already the one its superclasses inherit.
//!
//! A superclass override, and a superclass that merely forwards the same default, both keep the
//! method. A subclass that adds a more specific interface override still forwards to it. The same
//! rule covers a generic method whose superclass override is a specialized method plus an erasure
//! bridge.

use super::common;

#[test]
fn a_subclass_keeps_the_superclass_interface_member() {
    let src = r#"
interface BaseDefault {
    val test: Int
        get() = 1

    fun id(value: Int): Int = value
}

interface MidDefault : BaseDefault

interface SpecificDefault : BaseDefault {
    override val test: Int
        get() = 2

    override fun id(value: Int): Int = value + 2
}

open class InheritsDefault : BaseDefault

open class OverridesDefault : BaseDefault {
    override val test: Int = super.test + 1

    override fun id(value: Int): Int = super.id(value) + 1
}

open class DiamondInherits : MidDefault, InheritsDefault()

open class DiamondOverrides : MidDefault, OverridesDefault()

open class RelistsDefault : BaseDefault, InheritsDefault()

open class RelistsOverride : BaseDefault, OverridesDefault()

open class MoreSpecific : SpecificDefault, InheritsDefault()

open class InheritsSpecific : SpecificDefault

open class RelistsBaseOverSpecific : BaseDefault, InheritsSpecific()

interface GenericDefault<T> {
    fun test(x: T): T = x
}

interface GenericMid : GenericDefault<Int>

open class GenericOverride : GenericDefault<Int> {
    override fun test(x: Int): Int = super.test(x) + 1
}

open class GenericDiamond : GenericMid, GenericOverride()

class GenericRelist : GenericDefault<Int>, GenericOverride()
"#;
    common::assert_classes_identical_to_kotlinc(
        "InterfaceDefaultSuperclass",
        src,
        &[
            "BaseDefault",
            "MidDefault",
            "SpecificDefault",
            "InheritsDefault",
            "OverridesDefault",
            "DiamondInherits",
            "DiamondOverrides",
            "RelistsDefault",
            "RelistsOverride",
            "MoreSpecific",
            "InheritsSpecific",
            "RelistsBaseOverSpecific",
            "GenericDefault",
            "GenericMid",
            "GenericOverride",
            "GenericDiamond",
            "GenericRelist",
        ],
    );
}
