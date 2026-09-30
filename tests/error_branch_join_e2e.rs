//! An unresolved conditional branch does not discard the other branch's type.

use super::common;
use super::diagnostics_parity_support::errors;

#[test]
fn an_unresolved_conditional_branch_keeps_the_other_branch_type() {
    let use_site = "\
package demo\n\
\n\
import java.util.concurrent.ConcurrentHashMap\n\
\n\
fun read(flag: Boolean, key: Class<*>): String {\n\
    val cache = if (flag) ConcurrentHashMap<Class<*>, String>() else Missing.create<Class<*>, String>()\n\
    val found = cache.get(key)\n\
    return found ?: \"\"\n\
}\n\
\n\
private val stored =\n\
    if (enabled()) ConcurrentHashMap<Class<*>, String>() else Missing.create<Class<*>, String>()\n\
\n\
fun enabled(): Boolean = false\n\
\n\
fun readStored(key: Class<*>): String {\n\
    val found = stored.get(key)\n\
    return found ?: \"\"\n\
}\n\
\n\
fun takeInt(value: Int) = value\n\
\n\
fun rejected(flag: Boolean) = takeInt(if (flag) \"ok\" else Missing.x)\n\
\n\
fun fromNull(flag: Boolean) = takeInt(if (flag) Missing.x else null)\n\
\n\
fun bothMissing(flag: Boolean) = takeInt(if (flag) Missing.x else Missing.y)\n\
\n\
fun fromWhen(flag: Boolean) = takeInt(when {\n\
    flag -> Missing.x\n\
    else -> \"no\"\n\
})\n\
\n\
fun fromElvis(value: String?) = takeInt(value ?: Missing.x)\n\
";
    let extension = "\
package demo\n\
\n\
fun <T : Any> Any.get(key: Any): T = throw IllegalStateException()\n\
";
    let result = common::compiler_diagnostics(
        &[("Use.kt", use_site), ("Extension.kt", extension)],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    assert_eq!(result.krusty_code, result.reference_code);
    assert_eq!(
        krusty, reference,
        "krusty:\n{krusty:?}\nkotlinc:\n{reference:?}"
    );
}
