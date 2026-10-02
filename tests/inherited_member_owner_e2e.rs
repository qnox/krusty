//! kotlinc calls an inherited member through the fake override in the dispatch receiver's own
//! class: `Leaf.m`, `Leaf.getBase`, `Leaf.hashCode`, an interface default through a class receiver
//! as `invokevirtual Leaf.shared`, and a dependency's member through a module or dependency subclass.
//! An interface receiver keeps a class member's declaration (`Object.toString`).

use super::common;

#[test]
fn inherited_members_are_called_through_the_receivers_class_like_kotlinc() {
    let src = r#"
open class Base(val base: Int) {
    var level = 0
    fun plain() = 1
    open fun open() = 2
}

interface Face {
    fun shared() = 3
    fun declared(): Int
}

open class Mid(start: Int) : Base(start), Face {
    override fun declared() = 4
}

class Leaf(start: Int) : Mid(start) {
    val sum = base + plain()
    fun total() = base + plain() + open() + shared() + declared()
}

fun members(leaf: Leaf): Int {
    leaf.level = 5
    return leaf.base + leaf.level + leaf.plain() + leaf.open() + leaf.shared() + leaf.declared()
}

fun identity(leaf: Leaf): Int = leaf.hashCode()

fun narrowed(value: Any): Int = if (value is Leaf) value.plain() else 0

fun faced(face: Face): String = face.toString()

private class Hidden(start: Int) : Base(start)

private fun hidden(value: Hidden): Int = value.plain()

fun lambda(leaf: Leaf): () -> Int = { leaf.plain() }

fun localOwner(): Int {
    class Local : Base(0)
    return Local().plain()
}

fun anonymousOwner(): Int {
    val local = object : Base(0) {}
    return local.plain()
}

fun anonymousInterfaceOwner(): Int {
    val local = object : Face {
        override fun declared() = 4
    }
    return local.shared()
}
"#;
    common::assert_classes_identical_to_kotlinc(
        "InheritedMemberOwner",
        src,
        &[
            "Base",
            "Face",
            "Mid",
            "Leaf",
            "Hidden",
            "InheritedMemberOwnerKt",
        ],
    );
}

const FRAMES: &str = r#"
package frames

open class Frame {
    fun size(): Int = 1
    open fun weight(): Int = 2
    val mark: Int get() = 3
    var tally: Int = 0
}

interface Part {
    fun shared(): Int = 4
}

open class Middle : Frame(), Part
"#;

/// A dependency's member is called through the receiver's class the same way, whether a module
/// class (`Piece.size`) or a dependency class (`Middle.size`) inherits it.
#[test]
fn a_dependencys_inherited_members_are_called_through_the_receivers_class_like_kotlinc() {
    let library =
        common::kotlinc_library(FRAMES).expect("reference compiler builds the dependency");
    let src = r#"
import frames.*

class Piece : Middle()

fun calls(piece: Piece): Int {
    piece.tally = 1
    return piece.size() + piece.weight() + piece.mark + piece.tally + piece.shared()
}

fun middle(middle: Middle): Int = middle.size() + middle.shared()

fun part(part: Part): String = part.toString()
"#;
    common::assert_classes_identical_to_kotlinc_against(
        "DependencyMemberOwner",
        src,
        &["Piece", "DependencyMemberOwnerKt"],
        &[library],
    );
}

/// The receiver's class names only a member it inherits: an annotation's member is an interface
/// call, and a receiver held across reordered named arguments keeps its own class.
#[test]
fn an_annotation_member_and_a_held_receiver_keep_kotlincs_owner() {
    let src = r#"
annotation class Tag(val weight: Int)

fun weight(tag: Tag): Int = tag.weight

abstract class Picker {
    fun pick(a: Int, b: Int, c: Int) = a * 100 + b * 10 + c
}

object Chosen : Picker()

fun next(value: Int): Int = value

fun picked(): Int = Chosen.pick(c = next(3), a = next(1), b = next(2))
"#;
    common::assert_classes_identical_to_kotlinc("HeldMemberOwner", src, &["HeldMemberOwnerKt"]);
}

/// An enum entry's own member reached through the enum's type is the entry class's, not an
/// inherited member of the enum.
#[test]
fn an_enum_entrys_own_member_keeps_its_class() {
    let src = r#"
enum class Mode {
    FIRST {
        val label = "OK"

        inner class Reader {
            fun read() = label
        }

        override val shown = Reader().read()
    };

    abstract val shown: String
}

fun box(): String = Mode.FIRST.shown
"#;
    common::expect_box_ok_with_stdlib(src, "EntryMemberOwner");
}
