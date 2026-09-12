//! The get-or-create prologue every suspend function opens with: `$completion instanceof Cont &&
//! (label & MIN_VALUE) != 0` reuses the continuation, and every read of the reused one loads it from
//! a local kotlinc casts ONCE. krusty re-cast per use, so the prologue carried three extra
//! `aload; checkcast` pairs in every suspend function in a program.
use super::common;

const SRC: &str = "package demo\n\
                   class Store {\n\
                   \x20 suspend fun find(name: String): String? = name\n\
                   }\n\
                   class Service(private val store: Store) {\n\
                   \x20 suspend fun create(name: String, kind: Int): String {\n\
                   \x20   val existing = store.find(name)\n\
                   \x20   if (existing != null) return existing\n\
                   \x20   return store.find(name + kind) ?: name\n\
                   \x20 }\n\
                   }\n";

/// How many times `method` casts to the continuation class.
fn continuation_casts(disassembly: &str, method: &str) -> usize {
    disassembly
        .lines()
        .skip_while(|line| !line.contains(method))
        .skip(1)
        .take_while(|line| !line.trim().ends_with(");"))
        .filter(|line| line.contains("checkcast") && line.contains("$create$1"))
        .count()
}

#[test]
fn the_get_or_create_prologue_casts_the_completion_once() {
    let Some(dir) = common::scratch_dir() else {
        eprintln!("skipping: no scratch dir");
        return;
    };
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).expect("reference output directory");
    std::fs::create_dir_all(&krusty_dir).expect("krusty output directory");
    let source = dir.join("Prologue.kt");
    std::fs::write(&source, SRC).expect("write fixture");

    let Some((code, stderr)) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");

    let classes = common::compile_in_process_metadata_cp_module_target(
        SRC,
        "Prologue",
        &[common::stdlib_jar(), common::jdk_modules()],
        "main",
        Some(69),
    )
    .expect("krusty compiles the fixture");
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("krusty class directory");
        }
        std::fs::write(path, bytes).expect("write krusty class");
    }

    let Some(reference) = common::javap(&[
        "-p",
        "-c",
        "-cp",
        &reference_dir.to_string_lossy(),
        "demo.Service",
    ]) else {
        eprintln!("skipping: javap unavailable");
        return;
    };
    let krusty = common::javap(&[
        "-p",
        "-c",
        "-cp",
        &krusty_dir.to_string_lossy(),
        "demo.Service",
    ])
    .expect("javap reads krusty's output");
    let _ = std::fs::remove_dir_all(&dir);

    let method = "java.lang.Object create(";
    let want = continuation_casts(&reference, method);
    assert_eq!(
        want, 1,
        "reference must cast once — that is the rule under test:\n{reference}"
    );
    assert_eq!(
        continuation_casts(&krusty, method),
        want,
        "continuation casts in the prologue\n{krusty}"
    );
}
