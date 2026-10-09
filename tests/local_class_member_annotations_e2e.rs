//! Annotations on the members of a local class and of an anonymous object. Those classifiers are
//! checked with the body that declares them, so their declaration metadata comes from that body's
//! checked result; a separate bodyless pass would see no checked application at all.
use super::common;

const SOURCE: &str = "abstract class Source { abstract fun get(): String }\n\
fun anonymous(): String {\n\
    val source = object : Source() {\n\
        @Deprecated(\"anonymous\")\n\
        override fun get(): String = \"O\"\n\
    }\n\
    val kept = source.javaClass.getMethod(\"get\").isAnnotationPresent(Deprecated::class.java)\n\
    return if (kept) source.get() else \"lost\"\n\
}\n\
fun local(): String {\n\
    class Local {\n\
        @Suppress(\"UNUSED\")\n\
        val value: String = \"K\"\n\
        @Deprecated(\"member\")\n\
        fun get(): String = value\n\
    }\n\
    val kept = Local::class.java.getMethod(\"get\").isAnnotationPresent(Deprecated::class.java)\n\
    return if (kept) Local().get() else \"lost\"\n\
}\n\
fun box(): String = anonymous() + local()\n";

#[test]
fn local_and_anonymous_class_members_keep_their_annotations() {
    common::expect_box_same_as_kotlinc(SOURCE, "LocalClassMemberAnnotations");
}
