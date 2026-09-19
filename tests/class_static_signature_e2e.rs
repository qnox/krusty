//! A CLASS-owned static field's generic `Signature`.
//!
//! Three sites decide whether a field records one — the file facade, an instance field, and a
//! static owned by a class — and only the first two asked. The class-owned path wrote through the
//! one field writer that takes no signature argument, so a parameterized static declared nothing
//! but its erasure.
//!
//! The rule is not serialization's: any `@JvmField` companion property lands on the outer class as
//! a static, and every one whose Kotlin type carries type arguments was wrong.
//!
//! Every classifier in the fixture is declared BY the fixture — `Cargo`, `Crate<T>`, `Vault<K, V>`.
//! Nothing here is a stdlib type, so a formatter path special-cased for builtins cannot make these
//! assertions pass; the rule has to hold for an arbitrary resolved generic classifier.
//!
//! Every step fails closed. A missing reference compiler or a failed krusty compile is a test
//! failure, not a skip: a signature regression that reports success is worse than no test.
use std::path::PathBuf;
use std::sync::OnceLock;

use super::common;

/// `@JvmField` companion properties are emitted on the OUTER class as `public static final` fields
/// — the plainest class-owned static Kotlin has.
///
/// `manifest` carries one type argument, `ledger` carries a type argument nested inside another,
/// and `tally` carries none at all and is the control: the rule is "erasure loses something", not
/// "the field is a static".
const SRC: &str = "class Cargo\n\
                   \n\
                   class Crate<T>(val item: T)\n\
                   \n\
                   class Vault<K, V>\n\
                   \n\
                   class Coffer {\n\
                   \x20   companion object {\n\
                   \x20       @JvmField\n\
                   \x20       val manifest: Crate<Cargo> = Crate(Cargo())\n\
                   \x20\n\
                   \x20       @JvmField\n\
                   \x20       val ledger: Vault<Cargo, Crate<Cargo>> = Vault()\n\
                   \x20\n\
                   \x20       @JvmField\n\
                   \x20       val tally: Cargo = Cargo()\n\
                   \x20   }\n\
                   }\n";

/// kotlinc's `Coffer` and krusty's, disassembled, both built for one `-jvm-target`.
struct Built {
    reference: String,
    krusty: String,
}

fn built() -> &'static Built {
    static BUILT: OnceLock<Built> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = common::scratch_dir().expect("a scratch directory for the fixture");
        let reference_dir = dir.join("ref");
        let krusty_dir = dir.join("out");
        std::fs::create_dir_all(&reference_dir).expect("create the reference output directory");
        std::fs::create_dir_all(&krusty_dir).expect("create the krusty output directory");
        let source = dir.join("Coffer.kt");
        std::fs::write(&source, SRC).expect("write the fixture source");

        let (code, stderr) = common::kotlinc_compile(&[
            "-d".to_string(),
            reference_dir.to_string_lossy().into_owned(),
            "-jvm-target".to_string(),
            "25".to_string(),
            source.to_string_lossy().into_owned(),
        ])
        .expect("the reference compiler must be available to this regression");
        assert_eq!(code, 0, "kotlinc failed to build the fixture: {stderr}");

        let classes = common::compile_in_process_metadata_cp_module_target(
            SRC,
            "Coffer",
            &[common::stdlib_jar()],
            "main",
            Some(69),
        )
        .expect("krusty must compile the fixture");
        for (internal, bytes) in &classes {
            let path = krusty_dir.join(format!("{internal}.class"));
            std::fs::create_dir_all(path.parent().expect("a class file has a parent directory"))
                .expect("create the class output directory");
            std::fs::write(path, bytes).expect("write the emitted class");
        }

        let disassemble = |dir: &PathBuf| {
            common::javap(&["-p", "-c", "-v", "-cp", &dir.to_string_lossy(), "Coffer"])
                .expect("javap must be available to this regression")
        };
        Built {
            reference: disassemble(&reference_dir),
            krusty: disassemble(&krusty_dir),
        }
    })
}

/// The field table as javap prints it, with constant-pool indices erased because their numbering is
/// an emission-order artifact: every declaration from the first field to the constructor.
fn fields(disassembly: &str) -> Vec<String> {
    disassembly
        .lines()
        .map(str::trim)
        .skip_while(|line| !line.contains(" manifest;"))
        .take_while(|line| !line.contains("Coffer();"))
        .map(|line| {
            let mut out = String::new();
            let mut rest = line;
            while let Some(at) = rest.find('#') {
                out.push_str(&rest[..at]);
                out.push('#');
                rest = rest[at + 1..].trim_start_matches(|c: char| c.is_ascii_digit());
            }
            out.push_str(rest);
            out
        })
        .collect()
}

/// The `Signature:` javap prints for one field, with its constant-pool index erased.
fn field_signature(disassembly: &str, field: &str) -> Option<String> {
    disassembly
        .lines()
        .skip_while(|line| !line.contains(&format!(" {field};")))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| {
            line.trim()
                .strip_prefix("Signature:")?
                .split("// ")
                .nth(1)
                .map(str::to_string)
        })
}

/// The COMPLETE field table matches kotlinc's — every attribute of every field, in order.
///
/// Comparing the table rather than the whole class is deliberate and narrow: `Coffer` still differs
/// from kotlinc by two constant-pool entries and two `<clinit>` line-table rows, neither of which
/// this rule reaches. The field table is exactly what the rule decides.
#[test]
fn a_class_owned_static_field_table_matches_kotlinc() {
    let built = built();
    assert_eq!(
        fields(&built.krusty),
        fields(&built.reference),
        "the class-owned static field table must match kotlinc's"
    );
}

/// The literal signatures, spelled out on BOTH sides — so a change in what the reference writes is
/// visible here rather than silently agreed with, and so a type argument NESTED inside a type
/// argument is pinned rather than extrapolated from one flat case.
#[test]
fn a_parameterized_class_owned_static_records_its_signature_literally() {
    let built = built();
    for (field, want) in [
        ("manifest", "LCrate<LCargo;>;"),
        ("ledger", "LVault<LCargo;LCrate<LCargo;>;>;"),
    ] {
        assert_eq!(
            field_signature(&built.reference, field).as_deref(),
            Some(want),
            "the reference's own record for `{field}`:\n{}",
            built.reference
        );
        assert_eq!(
            field_signature(&built.krusty, field).as_deref(),
            Some(want),
            "krusty must record the same for `{field}`:\n{}",
            built.krusty
        );
    }
}

/// A static whose Kotlin type has NO type arguments records nothing. Without this the fix could be
/// a blanket attribute and the table above would still match on the two parameterized rows.
#[test]
fn a_class_owned_static_with_no_type_arguments_records_no_signature() {
    let built = built();
    assert_eq!(
        field_signature(&built.reference, "tally"),
        None,
        "the reference records none for the non-generic `Cargo`:\n{}",
        built.reference
    );
    assert_eq!(
        field_signature(&built.krusty, "tally"),
        None,
        "krusty must record none either:\n{}",
        built.krusty
    );
}
