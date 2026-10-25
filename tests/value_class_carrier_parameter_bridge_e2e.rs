//! A bridge to a value class's own member for a generic supertype member that also declares a
//! value-class parameter of its own.
//!
//! `Store<T>.put(id: Id, value: T)` is `put-<hash>(ILjava/lang/Object;)` on the interface: its
//! own `Id` parameter mangles the name and is passed as `Id`'s carrier, while `T` erases to
//! `Object`. A value class implementing `Store<Id>` keeps its member as a static over the carrier
//! and declares a bridge on the box under the interface's mangled name. Only the `T` slot arrives
//! boxed: the bridge unboxes that argument and passes the `Id` carrier (an `int`, or the `String`
//! of a reference-backed `Name`) through as it is. An ordinary class implementing the same
//! interface takes the same bridge.

use super::common;

const SOURCE: &str = r#"@JvmInline value class Id(val v: Int)
@JvmInline value class Name(val s: String)

interface Store<T> {
    fun put(id: Id, value: T): String
    fun label(n: Name, value: T): String
    fun opt(n: Name?, value: T): String
    fun next(id: Id, value: T): Id
    fun wrap(id: Id, value: T): T
}

@JvmInline value class Shelf(val tag: String) : Store<Id> {
    override fun put(id: Id, value: Id): String = tag + id.v + ":" + value.v
    override fun label(n: Name, value: Id): String = tag + n.s + value.v
    override fun opt(n: Name?, value: Id): String = tag + (n?.s ?: "-") + value.v
    override fun next(id: Id, value: Id): Id = Id(id.v + value.v)
    override fun wrap(id: Id, value: Id): Id = Id(id.v * value.v)
}

class Rack(val tag: String) : Store<Name> {
    override fun put(id: Id, value: Name): String = tag + id.v + ":" + value.s
    override fun label(n: Name, value: Name): String = tag + n.s + value.s
    override fun opt(n: Name?, value: Name): String = tag + (n?.s ?: "-") + value.s
    override fun next(id: Id, value: Name): Id = Id(id.v + value.s.length)
    override fun wrap(id: Id, value: Name): Name = Name(value.s + id.v)
}

fun box(): String {
    val shelf: Store<Id> = Shelf("a")
    val rack: Store<Name> = Rack("b")
    val got = shelf.put(Id(1), Id(2)) + shelf.label(Name("x"), Id(3)) + shelf.opt(null, Id(4)) +
        shelf.next(Id(5), Id(6)).v + shelf.wrap(Id(2), Id(7)).v +
        rack.put(Id(8), Name("y")) + rack.label(Name("q"), Name("w")) + rack.opt(Name("e"), Name("r")) +
        rack.next(Id(1), Name("ab")).v + rack.wrap(Id(9), Name("k")).s
    return if (got == "a1:2ax3a-41114b8:ybqwber3k9") "OK" else got
}
"#;

#[test]
fn the_bridges_are_kotlincs_classes() {
    common::assert_classes_identical_to_kotlinc(
        "CarrierParameterBridge",
        SOURCE,
        &["Shelf", "Rack", "Store"],
    );
}

#[test]
fn calls_through_the_interface_reach_the_overrides() {
    common::expect_box_ok_with_stdlib(SOURCE, "CarrierParameterBridge");
}
