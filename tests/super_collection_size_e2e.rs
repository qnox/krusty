//! `super.size` on an `ArrayList` subclass calls `java.util.ArrayList.size()I`.
//!
//! The private `size` field is JVM storage, not the Kotlin property. Reading it with `getfield`
//! is an `IllegalAccessError` (`specialBuiltins/explicitSuperCall.kt`).

use super::common;
use std::fs;

const SRC: &str = "class A : ArrayList<String>() {\n\
    override val size: Int get() = super.size + 56\n\
}\n\
fun box(): String {\n\
    val a = A()\n\
    if (a.size != 56) return \"fail: ${a.size}\"\n\
    return \"OK\"\n\
}\n";

fn getter_instructions(root: &std::path::Path, class_bytes: &[u8]) -> Vec<String> {
    let class_file = root.join("A.class");
    fs::write(&class_file, class_bytes).expect("write A.class");
    let disassembly = common::javap(&["-c", "-p", "-v", &class_file.to_string_lossy()])
        .expect("javap unavailable");
    let instructions = common::method_instructions(&disassembly, "int getSize();");
    assert!(!instructions.is_empty(), "missing getSize:\n{disassembly}");
    instructions
}

#[test]
fn array_list_super_size_calls_size() {
    common::expect_box_ok_with_stdlib(SRC, "SuperCollectionSize");

    let root = common::scratch_dir().expect("scratch directory");
    let reference_dir = root.join("ref");
    fs::create_dir_all(&reference_dir).expect("reference output");
    let source = root.join("SuperCollectionSize.kt");
    fs::write(&source, SRC).expect("write fixture");
    let stdlib = common::stdlib_jar();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-classpath".to_string(),
        stdlib.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let reference_bytes = fs::read(reference_dir.join("A.class")).expect("kotlinc A.class");
    let krusty_bytes = common::expect_classes_with_stdlib(SRC, "SuperCollectionSize")
        .into_iter()
        .find(|(name, _)| name == "A")
        .map(|(_, bytes)| bytes)
        .expect("krusty emits A");

    let krusty_dir = root.join("krusty");
    fs::create_dir_all(&krusty_dir).expect("krusty output");
    let reference = getter_instructions(&reference_dir, &reference_bytes);
    let krusty = getter_instructions(&krusty_dir, &krusty_bytes);
    assert_eq!(krusty, reference, "A.getSize");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn protected_super_property_is_reachable_from_a_subclass_lambda() {
    const SOURCE: &str = "open class A {\n\
        var state = \"\"\n\
        protected open fun method(): String = \"A.method\"\n\
        protected open var property: String\n\
            get() = \"A.property\"\n\
            set(value) { state += \"A.property;\" }\n\
    }\n\
    open class B : A() {\n\
        fun read(): String {\n\
            val overriddenMethod: () -> String = { method() }\n\
            val superMethod: () -> String = { super.method() }\n\
            val overriddenProperty: () -> String = { property }\n\
            val superProperty: () -> String = { super.property }\n\
            val overriddenSetter: () -> Unit = { property = \"\" }\n\
            val superSetter: () -> Unit = { super.property = \"\" }\n\
            overriddenSetter()\n\
            superSetter()\n\
            return overriddenMethod() + \":\" + superMethod() + \":\" + overriddenProperty() + \":\" + superProperty() + \":\" + state\n\
        }\n\
    }\n\
    class C : B() {\n\
        override fun method() = \"C.method\"\n\
        override var property: String\n\
            get() = \"C.property\"\n\
            set(value) { state += \"C.property;\" }\n\
    }\n\
    fun box(): String {\n\
        val got = C().read()\n\
        if (got != \"C.method:A.method:C.property:A.property:C.property;A.property;\") return got\n\
        return \"OK\"\n\
    }\n";
    common::expect_box_ok_with_stdlib(SOURCE, "ProtectedSuperLambda");
}

#[test]
fn protected_super_property_with_private_setter_is_readable() {
    const SOURCE: &str = "open class A {\n\
        protected var vo = \"O\"\n\
            private set\n\
        protected var vk = \"\"\n\
            private set\n\
        fun fk() = { ->\n\
            vk = \"K\"\n\
            vk\n\
        }\n\
    }\n\
    class B : A() {\n\
        fun test() = { -> super.vo + fk()() }\n\
    }\n\
    fun box() = B().test()()\n";
    common::expect_box_ok_with_stdlib(SOURCE, "ProtectedSuperPrivateSetter");
}

#[test]
fn private_super_setter_stays_unwritable() {
    const SOURCE: &str = "open class A {\n\
        protected var vo = \"O\"\n\
            private set\n\
    }\n\
    class B : A() {\n\
        fun write() { super.vo = \"X\" }\n\
    }\n";
    assert_eq!(
        common::front_end_diagnostics(SOURCE, &[], None),
        vec!["unresolved writable super property 'vo'".to_string()]
    );
}
