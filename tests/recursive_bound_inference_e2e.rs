//! Inference over a type parameter whose upper bound names itself (`out T : Out<T>`). Solving the
//! call `o.copy(x)` on `Out<Out<*>>` binds `T` to a type that names `T` again; applying that binding
//! must expand it once, as kotlinc does, rather than re-substitute it forever.

use super::common;

#[test]
fn a_call_on_a_self_bounded_receiver_finishes_and_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "\
interface Out<out T : Out<T>> {
    fun copy(t: @UnsafeVariance T): Out<T>
}

class Node : Out<Node> {
    override fun copy(t: Node): Out<Node> = t
}

fun copyStar(star: Out<*>, nested: Out<Out<*>>) = nested.copy(star)

fun box(): String {
    val node = Node()
    @Suppress(\"UNCHECKED_CAST\")
    val nested = node as Out<Out<*>>
    return if (copyStar(node, nested) === node) \"OK\" else \"copy returned another instance\"
}
",
        "SelfBoundedCopy",
    );
}
