//! A local class that writes or reads a private member-extension property of its enclosing class
//! reaches the accessor through `access$<name>`. The accessor is a private instance method, and
//! the local class is a separate class file.
//!
//! Owned bytecode fixtures use repository classifiers `Key` and `Element` as the extension
//! receivers. `String` remains only where the property value or `box()` result is text. The corpus
//! case keeps kotlinc's `Int` receiver.

use super::common::{self, ReferenceComparison};

#[test]
fn local_class_writes_private_member_extension() {
    // `extensionProperties/accessorForPrivateSetter.kt` — the corpus spelling, including `Int`.
    common::expect_box_ok_with_stdlib(
        "class A {\n\
             var result = \"Fail\"\n\
             private var Int.foo: String\n\
                 get() = result\n\
                 private set(value) { result = value }\n\
             fun run(): String {\n\
                 class O {\n\
                     fun run() { 42.foo = \"OK\" }\n\
                 }\n\
                 O().run()\n\
                 return (-42).foo\n\
             }\n\
         }\n\
         fun box() = A().run()\n",
        "accessorForPrivateSetter",
    );
}

#[test]
fn local_class_reads_private_member_extension() {
    common::expect_box_ok_with_stdlib(
        "class Key\n\
         class B {\n\
             private val Key.item: String\n\
                 get() = \"OK\"\n\
             fun run(): String {\n\
                 class O {\n\
                     fun run() = Key().item\n\
                 }\n\
                 return O().run()\n\
             }\n\
         }\n\
         fun box() = B().run()\n",
        "privateMemberExtensionRead",
    );
}

fn compare(name: &str, src: &str, class: &str) -> ReferenceComparison {
    common::compare_with_kotlinc_plugin(name, src, class, &[common::stdlib_jar()], "17", &[])
        .expect("reference kotlinc and javap are provisioned")
}

fn assert_same_method(built: &ReferenceComparison, header: &str) {
    let reference = common::method_block(&built.reference, header);
    assert!(
        !reference.is_empty(),
        "kotlinc declares {header}: {}",
        built.reference
    );
    assert_eq!(
        common::method_block(&built.krusty, header),
        reference,
        "{header}"
    );
}

const HOST: &str = "class Key\n\
    class Element\n\
    class Host {\n\
        private val Key.item: String\n\
            get() = \"OK\"\n\
        private var Element.mark: String\n\
            get() = \"x\"\n\
            private set(value) {}\n\
        fun own(): String = Key().item\n\
        fun nested(): String {\n\
            class Local {\n\
                fun read() = Key().item\n\
                fun write() { Element().mark = \"K\" }\n\
            }\n\
            val local = Local()\n\
            local.write()\n\
            return local.read()\n\
        }\n\
    }\n\
    fun box(): String {\n\
        val host = Host()\n\
        return if (host.own() == \"OK\" && host.nested() == \"OK\") \"OK\" else \"fail\"\n\
    }\n";

/// The bridge name, descriptor, `public static final synthetic` flags, and `invokespecial` body
/// match kotlinc. The local class calls that bridge; the owner's own call stays `invokespecial`
/// of the private accessor.
#[test]
fn a_local_class_reaches_private_accessors_through_their_bridges() {
    let host = compare("MemberExtensionBridge", HOST, "Host");
    assert_same_method(
        &host,
        "public static final java.lang.String access$getItem(Host, Key);",
    );
    assert_same_method(
        &host,
        "public static final void access$setMark(Host, Element, java.lang.String);",
    );
    assert_same_method(&host, "public final java.lang.String own();");
    let local = compare("MemberExtensionBridge", HOST, "Host$nested$Local");
    assert_same_method(&local, "public final java.lang.String read();");
    assert_same_method(&local, "public final void write();");
    common::expect_box_same_as_kotlinc(HOST, "MemberExtensionBridge");
}

const PUBLIC_CALL: &str = "class Key\n\
    class Pub {\n\
        val Key.label: String\n\
            get() = \"OK\"\n\
    }\n\
    class Reader {\n\
        fun read(pub: Pub): String = with(pub) { Key().label }\n\
    }\n\
    fun box(): String = if (Reader().read(Pub()) == \"OK\") \"OK\" else \"fail\"\n";

/// A public member extension called from another class is an ordinary `invokevirtual`. Neither
/// class declares an `access$` bridge for it.
#[test]
fn a_public_member_extension_called_from_another_class_stays_direct() {
    let reader = compare("PublicMemberExtension", PUBLIC_CALL, "Reader");
    let invoke_shape = |disassembly: &str| {
        common::method_instructions(disassembly, "String read(Pub);")
            .into_iter()
            .filter_map(|instruction| {
                let (_, operation) = instruction.split_once(": ")?;
                operation.contains("invoke").then(|| operation.to_string())
            })
            .collect::<Vec<_>>()
    };
    let calls = invoke_shape(&reader.krusty);
    assert_eq!(calls, invoke_shape(&reader.reference));
    assert!(
        calls.iter().any(|instruction| {
            instruction.contains("invokevirtual") && instruction.contains("Pub.getLabel")
        }),
        "the public accessor is a direct call: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|instruction| !instruction.contains("access$")),
        "a public accessor is not reached through a bridge: {calls:?}"
    );
    let owner = compare("PublicMemberExtension", PUBLIC_CALL, "Pub");
    let members = common::member_table(&owner.krusty_bytes);
    assert_eq!(members, common::member_table(&owner.reference_bytes));
    assert!(
        members.iter().any(|member| member.contains("getLabel")),
        "the public accessor is declared: {members:?}"
    );
    assert!(
        members.iter().all(|member| !member.contains("access$")),
        "a public accessor needs no bridge: {members:?}"
    );
    common::expect_box_same_as_kotlinc(PUBLIC_CALL, "PublicMemberExtension");
}

const INTERFACE: &str = "class Element\n\
    interface Face {\n\
        private val Element.token: String\n\
            get() = \"OK\"\n\
        fun read(): String {\n\
            class Local {\n\
                fun run() = Element().token\n\
            }\n\
            return Local().run()\n\
        }\n\
    }\n\
    class Use : Face\n\
    fun box(): String = if (Use().read() == \"OK\") \"OK\" else \"fail\"\n";

/// An interface bridge is `public static synthetic` (not `final`) and the local class calls it
/// through an `InterfaceMethodref`.
#[test]
fn an_interface_private_member_extension_uses_a_static_bridge() {
    let face = compare("InterfaceMemberExtension", INTERFACE, "Face");
    assert_same_method(
        &face,
        "public static java.lang.String access$getToken(Face, Element);",
    );
    let local = compare("InterfaceMemberExtension", INTERFACE, "Face$read$Local");
    assert_same_method(&local, "public final java.lang.String run();");
    common::expect_box_same_as_kotlinc(INTERFACE, "InterfaceMemberExtension");
}

/// A private member extension is not visible to an unrelated class. Both compilers report the
/// same unresolved reference.
#[test]
fn a_private_member_extension_of_an_unrelated_class_is_unresolved() {
    let source = "class Key\n\
        class Host {\n\
            private val Key.item: String\n\
                get() = \"OK\"\n\
        }\n\
        class Other {\n\
            fun read(): String = Key().item\n\
        }\n";
    let diagnostics = common::compiler_diagnostics(&[("Bad.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&diagnostics, "private member extension");
}
