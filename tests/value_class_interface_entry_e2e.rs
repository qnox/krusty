//! A value class implements an interface through ordinary instance entries on its box.
//!
//! Each member the interface calls is realized as a static replacement over the carrier
//! (`f-<hash>(carrier, params)`). kotlinc gives the box one public instance method per such member,
//! declared right after that replacement, with the member's physical name and parameters, its
//! generic `Signature` and nullability annotations less the carrier. The entry checks its parameters
//! (quoting an extension receiver as `<this>`), reads the carrier and calls the replacement; every
//! generic bridge calls the entry.

use super::common;

const SOURCE: &str = "@JvmInline value class S(val x: String)\n\
                      interface IFoo<T> {\n\
                      \x20   fun memberFun(s1: S, s2: String): String\n\
                      \x20   fun memberFunT(x1: T, x2: String): String\n\
                      \x20   fun <X> genericMemberFun(x1: T, x2: X): String\n\
                      \x20   fun S.memberExtFun(s: String): String\n\
                      }\n\
                      @JvmInline value class FooImpl(val v: String) : IFoo<S> {\n\
                      \x20   override fun memberFun(s1: S, s2: String): String = v + s1.x + s2\n\
                      \x20   override fun memberFunT(x1: S, x2: String): String = v + x1.x + x2\n\
                      \x20   override fun <X> genericMemberFun(x1: S, x2: X): String = v + x1.x + x2\n\
                      \x20   override fun S.memberExtFun(s: String): String = v + x + s\n\
                      }\n\
                      fun <X> generic(s: S, x: X): String = s.x + x\n\
                      class Test : IFoo<S> by FooImpl(\"1\")\n\
                      fun box(): String {\n\
                      \x20   val test = Test()\n\
                      \x20   if (test.memberFun(S(\"O\"), \"K\") != \"1OK\") return \"memberFun\"\n\
                      \x20   if (test.memberFunT(S(\"O\"), \"K\") != \"1OK\") return \"memberFunT\"\n\
                      \x20   if (test.genericMemberFun(S(\"O\"), \"K\") != \"1OK\") return \"generic\"\n\
                      \x20   if (with(test) { S(\"O\").memberExtFun(\"K\") } != \"1OK\") return \"ext\"\n\
                      \x20   val foo: IFoo<S> = FooImpl(\"2\")\n\
                      \x20   if (foo.memberFunT(S(\"O\"), \"K\") != \"2OK\") return \"bridge\"\n\
                      \x20   return generic(S(\"O\"), \"K\")\n\
                      }\n";

/// Both compilers' classes, disassembled verbosely with constant-pool indices normalized away (two
/// pools interned in different orders describe the same members).
fn disassembled(class: &str) -> (String, String) {
    let dir = common::scratch_dir().expect("a scratch directory");
    let (kotlinc, krusty) = (dir.join("kotlinc"), dir.join("krusty"));
    std::fs::create_dir_all(&kotlinc).expect("kotlinc output directory");
    std::fs::create_dir_all(&krusty).expect("krusty output directory");
    let source = dir.join("ValueClassInterfaceEntry.kt");
    std::fs::write(&source, SOURCE).expect("source file");
    let (status, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        kotlinc.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc is provisioned");
    assert_eq!(status, 0, "kotlinc failed: {stderr}");
    let classes = common::compile_in_process_metadata_cp(
        SOURCE,
        "ValueClassInterfaceEntry",
        &[common::stdlib_jar()],
    )
    .expect("krusty compiles the source");
    for (name, bytes) in classes {
        std::fs::write(krusty.join(format!("{name}.class")), bytes).expect("krusty class file");
    }
    let render = |root: &std::path::Path| {
        let text = common::javap(&["-p", "-v", "-cp", &root.to_string_lossy(), class])
            .expect("javap runs");
        let members = text
            .split_once("\n{")
            .map(|(_, members)| members)
            .expect("javap prints the members");
        members
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(|token| if token.starts_with('#') { "#" } else { token })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rendered = (render(&kotlinc), render(&krusty));
    let _ = std::fs::remove_dir_all(&dir);
    rendered
}

/// Each member's section, keyed by its header line, in class-file order.
fn members(disassembly: &str) -> Vec<(String, String)> {
    let mut members: Vec<(String, String)> = Vec::new();
    for section in disassembly.split("\n\n") {
        let mut lines = section.lines().filter(|line| !line.is_empty());
        if let Some(header) = lines.next() {
            members.push((header.to_string(), lines.collect::<Vec<_>>().join("\n")));
        }
    }
    members
}

/// An instance method under a mangled name: the static replacements are `public static`.
fn is_entry(header: &str) -> bool {
    header.starts_with("public ") && !header.starts_with("public static ") && header.contains("--")
}

/// The box declares kotlinc's members in kotlinc's order, and each entry matches kotlinc's in full:
/// flags, code, debug tables, `Signature` and nullability annotations.
#[test]
fn a_value_class_declares_kotlincs_interface_entries() {
    let (reference, actual) = disassembled("FooImpl");
    let (reference, actual) = (members(&reference), members(&actual));
    let headers = |members: &[(String, String)]| {
        members
            .iter()
            .map(|(header, _)| header.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(headers(&actual), headers(&reference));
    let entries = reference
        .iter()
        .filter(|(header, _)| is_entry(header))
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 4, "{entries:#?}");
    for entry in entries {
        let actual = actual
            .iter()
            .find(|(header, _)| *header == entry.0)
            .expect("krusty declares the entry");
        assert_eq!(actual.1, entry.1, "{}", entry.0);
    }
}

/// A generic function signs a value-class parameter as its carrier, as its descriptor does.
#[test]
fn a_generic_signature_signs_a_value_class_as_its_carrier() {
    let (reference, actual) = disassembled("ValueClassInterfaceEntryKt");
    let generic = |members: Vec<(String, String)>| {
        let (header, body) = members
            .into_iter()
            .find(|(header, _)| header.contains(" generic--"))
            .expect("the generic function is declared");
        let signature = body
            .lines()
            .find(|line| line.starts_with("Signature:"))
            .map(str::to_string);
        (header, signature)
    };
    assert_eq!(generic(members(&actual)), generic(members(&reference)));
}

#[test]
fn delegated_value_class_interface_members_run() {
    let output = common::compile_and_run_box(
        SOURCE,
        "ValueClassInterfaceEntryBox",
        &[common::stdlib_jar()],
        None,
    )
    .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
