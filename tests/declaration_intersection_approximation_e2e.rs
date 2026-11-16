//! An inferred declaration publishes its type with every intersection approximated to the
//! components' common supertype, as kotlinc's public-declaration approximation does, whatever the
//! declaration's visibility. A local variable keeps the intersection.

use super::common;

#[test]
fn an_inferred_declaration_publishes_its_intersections_common_supertype() {
    let source = "\
interface Base
interface Mid : Base
interface Zed : Base
class X : Mid, Zed
class Y : Mid, Zed
fun <T> choose(first: T, second: T): T = first
val top = choose(X(), Y())
private val hidden = choose(X(), Y())
val list = listOf(X(), Y())
fun function() = choose(X(), Y())
class Holder { private val member = choose(X(), Y()); fun read() { val s: String = member } }
fun use() {
    val local = choose(X(), Y())
    val s1: String = top
    val s2: String = hidden
    val s3: String = list
    val s4: String = function()
    val s5: String = local
}
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:11:82: initializer type mismatch: expected 'String', actual 'Base'.",
            "test.kt:14:20: initializer type mismatch: expected 'String', actual 'Base'.",
            "test.kt:15:20: initializer type mismatch: expected 'String', actual 'Base'.",
            "test.kt:16:20: initializer type mismatch: expected 'String', actual 'List<Base>'.",
            "test.kt:17:20: initializer type mismatch: expected 'String', actual 'Base'.",
            "test.kt:18:20: initializer type mismatch: expected 'String', actual 'Mid & Zed'.",
        ]
    );
}
