//! A parameterless `fun main()` gets kotlinc's synthetic `main([Ljava/lang/String;)V` bridge.
//!
//! The JVM launcher looks for `main(String[])`. The no-arg form is only recognized by JEP 445, which
//! is preview before Java 21 and final in 25 — so a file compiled without the bridge runs on a
//! current JDK and fails to launch on anything older. kotlinc emits the bridge; krusty emitted only
//! the no-arg method.
//!
//! DIFFERENTIAL: the same source goes through the provisioned kotlinc and through krusty, and the
//! emitted method set is compared.
use super::common;

/// One compiled class file: its internal name and bytes.
type Class = (String, Vec<u8>);

/// `(name, descriptor, access)` for every method of `class`, sorted — the facade's method SET, which
/// is what an entry-point bridge changes.
fn methods(classes: &[Class], class: &str) -> Vec<(String, String, u16)> {
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == class).then_some(bytes))
        .unwrap_or_else(|| panic!("missing {class}.class"));
    let parsed = krusty::jvm::classreader::parse_class(bytes)
        .unwrap_or_else(|_| panic!("parse {class}.class"));
    let mut out = parsed
        .methods
        .iter()
        .map(|m| (m.name.clone(), m.descriptor.clone(), m.access))
        .collect::<Vec<_>>();
    out.sort();
    out
}

fn compile_both(stem: &str, src: &str) -> Option<(Vec<Class>, Vec<Class>)> {
    let work = common::scratch_dir()?;
    let source = work.join(format!("{stem}.kt"));
    let output = work.join("out");
    std::fs::create_dir_all(&output).ok()?;
    std::fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        source.to_string_lossy().into_owned(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
    ])?;
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let mut reference = Vec::new();
    for entry in std::fs::read_dir(&output).ok()? {
        let path = entry.ok()?.path();
        if path.extension().is_some_and(|e| e == "class") {
            let name = path.file_stem()?.to_string_lossy().into_owned();
            reference.push((name, std::fs::read(&path).ok()?));
        }
    }
    reference.sort();
    let ours = common::compile_in_process(
        src,
        stem,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    )?;
    let _ = std::fs::remove_dir_all(work);
    Some((ours, reference))
}

/// The bridge exists, and the facade's whole method set matches kotlinc's.
#[test]
fn a_parameterless_main_gets_kotlincs_string_array_bridge() {
    let Some((ours, reference)) = compile_both("Entry", "fun main() { println(1) }\n") else {
        return; // toolchain not provisioned
    };
    let ours = methods(&ours, "EntryKt");
    let reference = methods(&reference, "EntryKt");
    assert!(
        ours.iter().any(|(name, desc, access)| name == "main"
                && desc == "([Ljava/lang/String;)V"
                // PUBLIC | STATIC | SYNTHETIC — the launcher's entry point, not a Kotlin declaration.
                && *access == 0x1009),
        "the synthetic launcher bridge must be emitted: {ours:?}"
    );
    assert_eq!(
        ours, reference,
        "the facade's method set must match kotlinc's"
    );
}

/// A declared `fun main(args: Array<String>)` IS the entry point: kotlinc emits no bridge beside it,
/// and a second `main([Ljava/lang/String;)V` would not even be a legal class file.
#[test]
fn a_declared_string_array_main_gets_no_bridge() {
    let Some((ours, reference)) = compile_both(
        "Args",
        "fun main(args: Array<String>) { println(args.size) }\n",
    ) else {
        return;
    };
    let ours = methods(&ours, "ArgsKt");
    let reference = methods(&reference, "ArgsKt");
    assert_eq!(
        ours.iter()
            .filter(|(name, desc, _)| name == "main" && desc == "([Ljava/lang/String;)V")
            .count(),
        1,
        "exactly one entry point, the declared one: {ours:?}"
    );
    assert_eq!(
        ours.iter().map(|(n, d, _)| (n, d)).collect::<Vec<_>>(),
        reference.iter().map(|(n, d, _)| (n, d)).collect::<Vec<_>>(),
        "the facade's method set must match kotlinc's"
    );
}
