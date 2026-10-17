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

fn assert_exact_access_error(
    diagnostics: &common::CompilerDiagnosticResult,
    expected: common::CompilerError,
) {
    assert_eq!(
        diagnostics.reference_code, 1,
        "{}",
        diagnostics.reference_stderr
    );
    assert_eq!(
        diagnostics.krusty_code, 1,
        "{}{}",
        diagnostics.krusty_stdout, diagnostics.krusty_stderr
    );
    let reference = common::compiler_errors(&diagnostics.reference_stderr);
    let krusty = common::compiler_errors(&diagnostics.krusty_stderr);
    assert_eq!(reference, [expected]);
    assert_eq!(krusty, reference);
    assert_eq!(common::compiler_errors(&diagnostics.krusty_stdout), []);
}

/// An inaccessible member-extension function renders its resolved extension receiver separately
/// from the named/defaulted value parameters. Both complete diagnostic ledgers are pinned: the
/// receiver must not consume `x`'s source name or default marker.
#[test]
fn an_inaccessible_member_extension_function_matches_kotlinc_exactly() {
    let source = r#"package demo

class C {
    private fun String.hidden(x: Int = 1): Int = x
}

fun use(c: C): Int = with(c) { "".hidden() }
"#;
    let diagnostics = common::compiler_diagnostics(&[("Main.kt", source)], &[common::stdlib_jar()]);
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Main.kt".to_string(),
            line: 7,
            column: 35,
            message:
                "cannot access 'fun String.hidden(x: Int = ...): Int': it is private in 'demo.C'."
                    .to_string(),
        },
    );
}

/// A local classifier has no source-level owner name in an access diagnostic. The stable local
/// declaration identity selects kotlinc's `file` container without reconstructing that fact from
/// the generated classifier spelling.
#[test]
fn an_inaccessible_local_member_extension_reports_the_file_container() {
    let source = r#"package demo

fun use(): Int {
    class Local {
        private fun String.hidden(): Int = length
    }
    val local = Local()
    return with(local) { "".hidden() }
}
"#;
    let diagnostics = common::compiler_diagnostics(&[("Main.kt", source)], &[common::stdlib_jar()]);
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Main.kt".to_string(),
            line: 8,
            column: 29,
            message: "cannot access 'fun String.hidden(): Int': it is private in file.".to_string(),
        },
    );
}

/// Context parameters are a declaration prefix, not ordinary value parameters. The selected
/// member's semantic shape supplies both the context split and every resolved parameter type.
#[test]
fn an_inaccessible_context_member_extension_matches_kotlinc_exactly() {
    let source = r#"package demo

class C {
    context(ctx: String)
    private fun String.hidden(x: Int = 1): Int = ctx.length + x
}

context(ctx: String)
fun use(c: C): Int = with(c) { "".hidden() }
"#;
    let diagnostics = common::compiler_diagnostics(&[("Main.kt", source)], &[common::stdlib_jar()]);
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Main.kt".to_string(),
            line: 9,
            column: 35,
            message: "cannot access 'context(ctx: String) fun String.hidden(x: Int = ...): Int': it is private in 'demo.C'."
                .to_string(),
        },
    );
}

/// A legacy context receiver has no source parameter name. Its recorded semantic role renders the
/// declaration as `context(String)`, never as the provider placeholder `context(_: String)`.
#[test]
fn an_inaccessible_legacy_context_member_extension_matches_kotlinc_exactly() {
    // Kotlin 2.4.20 removed the legacy `context(Type)` source form. Older supported targets still
    // exercise this declaration shape in their matrix lanes.
    if krusty::kotlin_version::target() >= krusty::kotlin_version::KotlinVersion::V2_4_20 {
        return;
    }
    let source = r#"// LANGUAGE: +ContextReceivers
package demo

class C {
    context(String)
    private fun String.hidden(x: Int = 1): Int = x
}

context(String)
fun use(c: C): Int = with(c) { "".hidden() }
"#;
    let arguments = common::language_directives::kotlinc_args(source);
    let diagnostics = common::compiler_diagnostics_with_reference_args(
        &[("Main.kt", source)],
        &[common::stdlib_jar()],
        &arguments,
    );
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Main.kt".to_string(),
            line: 10,
            column: 35,
            message: "cannot access 'context(String) fun String.hidden(x: Int = ...): Int': it is private in 'demo.C'."
                .to_string(),
        },
    );
}

/// A dependency suspend member extension retains a physical trailing `Continuation`, but source
/// diagnostics render only its resolved source slots and its suspend modifier.
#[test]
fn an_inaccessible_dependency_suspend_member_extension_matches_kotlinc_exactly() {
    let Some(library) = common::kotlinc_lib_out(&[(
        "Library.kt",
        r#"package demo

class C {
    private suspend fun String.hidden(x: Int = 1): Int = x
}
"#,
    )]) else {
        return;
    };
    let source = r#"package client

import demo.C

suspend fun use(c: C): Int = with(c) { "".hidden() }
"#;
    let diagnostics =
        common::compiler_diagnostics(&[("Use.kt", source)], &[library, common::stdlib_jar()]);
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Use.kt".to_string(),
            line: 5,
            column: 43,
            message: "cannot access 'suspend fun String.hidden(x: Int = ...): Int': it is private in 'demo.C'."
                .to_string(),
        },
    );
}

/// The selected declaration renderer retains generic formals, bounds, `reified`, and `vararg` from
/// the same semantic candidate. Access reporting must not reconstruct this shape from a specialized
/// argument vector or print the vararg's array representation.
#[test]
fn an_inaccessible_generic_vararg_member_extension_matches_kotlinc_exactly() {
    let source = r#"package demo

class C {
    private inline fun <reified T : CharSequence> T.hidden(vararg values: T): T = values[0]
}

fun use(c: C): CharSequence = with(c) { "".hidden("x") }
"#;
    let diagnostics = common::compiler_diagnostics(&[("Main.kt", source)], &[common::stdlib_jar()]);
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Main.kt".to_string(),
            line: 7,
            column: 44,
            message: "cannot access 'fun <reified T : CharSequence> T.hidden(vararg values: T): T': it is private in 'demo.C'."
                .to_string(),
        },
    );
}

/// Ordinary selected members use the same declaration renderer as member extensions. Keep this
/// separate so a receiver-slot repair cannot leave the direct member path's generic/vararg ledger
/// untested.
#[test]
fn an_inaccessible_generic_vararg_member_matches_kotlinc_exactly() {
    let source = r#"package demo

class C {
    private inline fun <reified T : CharSequence> hidden(vararg values: T): T = values[0]
}

fun use(c: C): CharSequence = c.hidden("x")
"#;
    let diagnostics = common::compiler_diagnostics(&[("Main.kt", source)], &[common::stdlib_jar()]);
    assert_exact_access_error(
        &diagnostics,
        common::CompilerError {
            file: "Main.kt".to_string(),
            line: 7,
            column: 33,
            message: "cannot access 'fun <reified T : CharSequence> hidden(vararg values: T): T': it is private in 'demo.C'."
                .to_string(),
        },
    );
}
