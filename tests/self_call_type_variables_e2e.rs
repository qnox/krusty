//! A call to a declaration from inside that declaration infers FRESH type variables.
//!
//! Type-parameter identities are declaration-owned, so inside `class Cell<E>` the constructor's
//! `E` and the class body's `E` are the same symbol. Kotlin still gives every call its own type
//! variable: `Cell(next)` with `next: E` constrains a fresh `E'` by the enclosing, fixed `E` and
//! solves `E' := E`. Reading that constraint as `E` against itself (no evidence) left the call's
//! variable unconstrained, so with no expected type it defaulted:
//!
//! ```text
//! error: return type mismatch: expected 'Cell<E>', actual 'Cell<Any?>'.
//! ```
//!
//! A generic function calling itself (`fun <V> V.chain()` calling `chain()` on its implicit
//! receiver) had the same conflation, reported as `cannot infer type for type parameter 'V'`.

use super::common;

/// Repository-owned shapes of the mechanism: constructors of the enclosing generic class with no
/// expected type (direct, nested, secondary, swapped, inside a lambda, as a generic argument), and
/// generic functions calling themselves through an argument and through an implicit receiver.
#[test]
fn a_call_to_the_enclosing_generic_declaration_infers_its_own_variables() {
    common::expect_box_same_as_kotlinc(
        "class Cell<E>(val item: E) {\n\
    fun same(next: E): Cell<E> {\n\
        val made = Cell(next)\n\
        return made\n\
    }\n\
    fun nested(next: E): Cell<Cell<E>> {\n\
        val inner = Cell(next)\n\
        val outer = Cell(inner)\n\
        return outer\n\
    }\n\
    fun inLambda(): Cell<E> {\n\
        val made = run { Cell(item) }\n\
        return made\n\
    }\n\
    fun asArgument(): List<Cell<E>> {\n\
        val both = listOf(Cell(item), this)\n\
        return both\n\
    }\n\
    fun <F> foreign(next: F): Cell<F> {\n\
        val made = Cell(next)\n\
        return made\n\
    }\n\
}\n\
class Couple<A, B>(val first: A, val second: B) {\n\
    constructor(first: A, second: B, tag: Int) : this(first, second)\n\
    fun copy(): Couple<A, B> {\n\
        val made = Couple(first, second, 0)\n\
        return made\n\
    }\n\
    fun swap(): Couple<B, A> {\n\
        val made = Couple(second, first)\n\
        return made\n\
    }\n\
}\n\
fun <V> repeated(value: V, times: Int): List<V> {\n\
    if (times == 0) return emptyList()\n\
    val rest = repeated(value, times - 1)\n\
    return rest + value\n\
}\n\
fun <V> V.chain(times: Int): List<V> {\n\
    if (times == 0) return emptyList()\n\
    val rest = chain(times - 1)\n\
    return rest + this\n\
}\n\
fun box(): String {\n\
    val cell = Cell(\"a\")\n\
    if (cell.same(\"b\").item != \"b\") return \"same\"\n\
    if (cell.nested(\"c\").item.item != \"c\") return \"nested\"\n\
    if (cell.inLambda().item != \"a\") return \"lambda\"\n\
    if (cell.asArgument().size != 2) return \"argument\"\n\
    if (cell.foreign(7).item != 7) return \"foreign\"\n\
    val couple = Couple(1, \"two\")\n\
    if (couple.copy().second != \"two\") return \"secondary\"\n\
    if (couple.swap().first != \"two\") return \"swap\"\n\
    if (repeated(\"r\", 3) != listOf(\"r\", \"r\", \"r\")) return \"recursive\"\n\
    if (\"x\".chain(2) != listOf(\"x\", \"x\")) return \"receiver\"\n\
    return \"OK\"\n\
}\n",
        "self_call_type_variables",
    );
}

/// The enclosing type is an ordinary lower constraint on the fresh variable, so it joins with the
/// call's other evidence (`V' := Any?` from `V` and `String`) rather than being ignored, which left
/// `String` alone and rejected the `V` argument.
#[test]
fn an_enclosing_type_joins_with_the_calls_other_evidence() {
    common::expect_box_same_as_kotlinc(
        "class Twin<A>(val left: A, val right: A) {\n\
    fun mixed(): Any? {\n\
        val made = Twin(left, \"s\")\n\
        return made.right\n\
    }\n\
}\n\
fun <V> joined(first: V, second: V, depth: Int): List<V> {\n\
    if (depth == 0) return listOf(first, second)\n\
    val widened = joined(first, \"s\", depth - 1)\n\
    if (widened.last() != \"s\") return emptyList()\n\
    val same = joined(first, second, depth - 1)\n\
    return same\n\
}\n\
fun box(): String {\n\
    if (Twin(1, 2).mixed() != \"s\") return \"constructor\"\n\
    if (joined(\"a\", \"b\", 2) != listOf(\"a\", \"b\")) return \"function\"\n\
    return \"OK\"\n\
}\n",
        "self_call_type_variable_join",
    );
}

/// Two calls to the same enclosing declaration in one contextual expression own independent
/// inference variables. The expected `Pair` slots may therefore complete them differently even
/// though both variables originate from the same declaration-owned `Box<T>` formal.
#[test]
fn sibling_calls_to_the_enclosing_declaration_have_distinct_variables() {
    common::expect_box_same_as_kotlinc(
        "class Box<T>(val value: T? = null) {\n\
    fun split(): Pair<Box<String>, Box<Int>> = Pair(Box(), Box())\n\
}\n\
fun box(): String {\n\
    val pair = Box<Unit>().split()\n\
    return if (pair.first.value == null && pair.second.value == null) \"OK\" else \"FAIL\"\n\
}\n",
        "self_call_type_variable_siblings",
    );
}

/// The constructed class is byte-identical to kotlinc's: the call's solution `Cell<E>` is what the
/// local's signature and the returned value carry.
#[test]
fn an_enclosing_constructor_call_emits_the_same_class_as_kotlinc() {
    let source = "class Cell<E>(val item: E) {\n\
    fun same(next: E): Cell<E> {\n\
        val made = Cell(next)\n\
        return made\n\
    }\n\
    fun nested(next: E): Cell<Cell<E>> {\n\
        val inner = Cell(next)\n\
        return Cell(inner)\n\
    }\n\
}\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "SelfConstructorCell",
        source,
        "Cell",
        &[common::stdlib_jar()],
    ) else {
        return;
    };
    result.unwrap();
}

/// The reported program: a generic tree whose builder methods construct `Tree(label)` inside
/// `Tree<T>` and whose companion constructs `Tree(root)` with its own `<T>`.
#[test]
fn the_generic_tree_builder_constructs_its_own_nodes() {
    let tree = "class TreeNode<T>(val value: T, var next: TreeNode<T>?, val first: TreeNode<T>?)\n\
\n\
class Tree<T>(val value: T) {\n\
    private var firstChild: Tree<T>? = null\n\
    private var lastChild: Tree<T>? = null\n\
    private var sibling: Tree<T>? = null\n\
\n\
    fun child(label: T, init: Tree<T>.() -> Unit): Tree<T> {\n\
        val node = Tree(label)\n\
        node.init()\n\
        appendChild(node)\n\
        return node\n\
    }\n\
\n\
    fun leaf(label: T): Tree<T> {\n\
        val node = Tree(label)\n\
        appendChild(node)\n\
        return node\n\
    }\n\
\n\
    private fun appendChild(node: Tree<T>) {\n\
        val tail = lastChild\n\
        if (tail == null) {\n\
            firstChild = node\n\
        } else {\n\
            tail.sibling = node\n\
        }\n\
        lastChild = node\n\
    }\n\
\n\
    fun walk(visit: (T, Int) -> Unit, depth: Int) {\n\
        visit(value, depth)\n\
        var c = firstChild\n\
        while (c != null) {\n\
            c.walk(visit, depth + 1)\n\
            c = c.sibling\n\
        }\n\
    }\n\
\n\
    fun walk(visit: (T, Int) -> Unit) {\n\
        walk(visit, 0)\n\
    }\n\
\n\
    companion object {\n\
        fun <T> of(root: T, init: Tree<T>.() -> Unit): Tree<T> {\n\
            val t = Tree(root)\n\
            t.init()\n\
            return t\n\
        }\n\
    }\n\
}\n";
    let main = "fun indent(d: Int): String {\n\
    var s = \"\"\n\
    var i = 0\n\
    while (i < d) {\n\
        s += \"  \"\n\
        i++\n\
    }\n\
    return s\n\
}\n\
\n\
fun box(): String {\n\
    val t = Tree.of(\"root\") {\n\
        child(\"alpha\") {\n\
            leaf(\"alpha-1\")\n\
            child(\"alpha-2\") {\n\
                leaf(\"alpha-2-a\")\n\
            }\n\
        }\n\
        child(\"beta\") {\n\
            leaf(\"beta-1\")\n\
        }\n\
        leaf(\"gamma\")\n\
    }\n\
    val out = StringBuilder()\n\
    t.walk({ v, d -> out.append(indent(d)).append(v).append('|') })\n\
    val walked = out.toString()\n\
    val expected = \"root|  alpha|    alpha-1|    alpha-2|      alpha-2-a|  beta|    beta-1|  gamma|\"\n\
    return if (walked == expected) \"OK\" else \"FAIL: \" + walked\n\
}\n";
    let sources = [("Tree.kt", tree), ("Main.kt", main)];
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "MainKt"),
        "OK",
        "kotlinc reference"
    );
    common::expect_box_ok_files_with_stdlib(&sources, "self_constructor_tree");
}
