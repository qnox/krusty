//! Classpath `protected` visibility: a Kotlin subclass of a javac-compiled Java base can call the
//! base's `protected` instance method (unqualified, through the inherited receiver) — kotlinc accepts
//! this, and the JVM allows the invoke because the caller is a subclass. krusty must surface the
//! `protected` member during the supertype member walk (not drop it as non-public) and resolve an
//! inherited classpath-superclass member of a user class at all.

use super::common;

#[test]
fn subclass_calls_protected_classpath_member() {
    // A Java base with a PROTECTED instance method — only reachable from a subclass.
    let base = "package lib;\npublic class Base {\n  protected int secret() { return 42; }\n}\n";
    // A Kotlin subclass calls the inherited protected member unqualified.
    let use_src = "import lib.Base\nclass Sub : Base() {\n  fun reveal(): Int = secret()\n}\n\
         fun box(): String {\n  if (Sub().reveal() != 42) return \"f1\"\n  return \"OK\"\n}\n";
    let out = common::java_interop_box("classpath_protected", &[("Base.java", base)], use_src);
    assert_eq!(out, "OK");
}

/// `java.lang.Object.clone()` is protected, so `column.clone()` on an arbitrary receiver cannot
/// call it. kotlinc ranks that inaccessible member below the enclosing class's private member
/// extension `Column.clone()`, which the plain and safe calls select.
const MEMBER_EXTENSION_CLONE: &str = "class Column<T>(val name: String)\n\
    class Alias {\n\
    \x20   private fun <T> Column<T>.clone() = Column<T>(name + \"K\")\n\
    \x20   fun fields(columns: List<Any>): List<Any> = columns.map { (it as? Column<*>)?.clone() ?: it }\n\
    \x20   fun copy(column: Column<Int>): Column<Int> = column.clone()\n\
    }\n\
    fun box(): String {\n\
    \x20   val alias = Alias()\n\
    \x20   val fields = alias.fields(listOf(Column<Int>(\"O\"), \"text\"))\n\
    \x20   if (fields[1] != \"text\") return \"Fail other\"\n\
    \x20   if (alias.copy(Column(\"O\")).name != \"OK\") return \"Fail copy\"\n\
    \x20   return (fields[0] as Column<*>).name\n\
    }\n";

#[test]
fn inaccessible_object_clone_yields_to_a_member_extension() {
    common::assert_class_code_matches_kotlinc(
        "MemberExtensionClone",
        MEMBER_EXTENSION_CLONE,
        "Alias",
    );
    common::expect_box_same_as_kotlinc(MEMBER_EXTENSION_CLONE, "MemberExtensionCloneRun");
}
