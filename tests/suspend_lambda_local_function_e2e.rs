//! A local function declared in a suspend lambda of a class member is lifted into the lambda's
//! class as `invokeSuspend$<name>`, as kotlinc does. Its calls were lowered as static calls on the
//! member's class before the lambda became a class, and must follow the selected function identity
//! to its final physical owner.

use super::common;

const SRC: &str = r#"import kotlin.coroutines.*

sealed class Node {
    data class Leaf(val text: String) : Node()
    data class Branch(val children: List<Node>) : Node()
}

fun <T> runSync(block: suspend () -> T): T {
    var result: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it })
    return result!!.getOrThrow()
}

class Walker {
    fun direct(): String = runSync {
        fun twice(s: String): String = s + s
        twice("O")
    }

    fun nested(): Node = runSync {
        val rewrite: suspend (Node, Int) -> Node = { node, _ ->
            fun recurse(n: Node): Node = when (n) {
                is Node.Leaf -> Node.Leaf(n.text + "!")
                is Node.Branch -> Node.Branch(n.children.map { recurse(it) })
            }
            recurse(node)
        }
        rewrite(Node.Branch(listOf(Node.Leaf("a"))), 0)
    }
}

fun box(): String {
    if (Walker().direct() != "OO") return "direct"
    if (Walker().nested() != Node.Branch(listOf(Node.Leaf("a!")))) return "nested"
    return "OK"
}
"#;

#[test]
fn a_local_function_of_a_member_suspend_lambda_is_called_on_the_lambda_class() {
    let classes = common::classes_against_kotlinc_module(&[("SuspendLocalFunction.kt", SRC)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    assert!(
        classes.krusty["Walker"] == classes.reference["Walker"],
        "Walker differs from kotlinc"
    );
}

#[test]
fn a_local_function_of_a_member_suspend_lambda_runs() {
    common::expect_box_same_as_kotlinc(SRC, "SuspendLambdaLocalFunction");
}
