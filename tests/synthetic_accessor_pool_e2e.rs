//! A synthetic accessor's name and descriptor join the constant pool as kotlinc visits the method,
//! before its body: a protected member's `access$…` bridge and a private object property's
//! `access$get…$p` keep kotlinc's pool order, so the classes are byte-identical.

use super::common::ModuleClassPair;

const BASE: &str = "\
package lib\n\
\n\
open class Base {\n\
    protected fun method(): String = \"m\"\n\
    protected var property: String = \"p\"\n\
}\n\
";

const SUB: &str = "\
package store\n\
\n\
import lib.Base\n\
\n\
fun call(f: () -> String): String = f()\n\
\n\
class SubClass : Base() {\n\
    fun reference() = ::method\n\
    fun viaLambda(): String = call { method() + property }\n\
    fun assign() {\n\
        call {\n\
            property = \"q\"\n\
            \"\"\n\
        }\n\
    }\n\
}\n\
\n\
object Holder {\n\
    private val text = \"OK\"\n\
\n\
    object Nested {\n\
        val copy = text\n\
    }\n\
}\n\
";

#[test]
fn synthetic_accessors_keep_kotlinc_pool_order() {
    for class in ["store/SubClass", "store/Holder"] {
        let pair = ModuleClassPair::compile(&[("base.kt", BASE), ("sub.kt", SUB)], class);
        assert!(pair.krusty == pair.kotlinc, "{class} differs from kotlinc");
    }
}
