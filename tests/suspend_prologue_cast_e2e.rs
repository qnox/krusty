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

/// The exact instruction shape through the resume-bit update. Constant-pool indices and branch
/// offsets are deliberately projected out; local slots are returned separately so the one known
/// layout mismatch cannot weaken the instruction-order assertion.
fn reuse_prefix(disassembly: &str) -> (Vec<String>, Vec<u16>) {
    let signature = "public final java.lang.Object create(java.lang.String, int, kotlin.coroutines.Continuation<? super java.lang.String>);";
    let mut lines = disassembly
        .lines()
        .skip_while(|line| line.trim() != signature)
        .skip(1)
        .skip_while(|line| line.trim() != "Code:")
        .skip(1);
    let mut instructions = Vec::new();
    let mut local_slots = Vec::new();
    for line in &mut lines {
        let Some((offset, instruction)) = line.trim().split_once(':') else {
            continue;
        };
        if offset.parse::<u32>().is_err() {
            continue;
        }
        let instruction = instruction.split("//").next().unwrap_or(instruction).trim();
        let mut words = instruction.split_whitespace();
        let raw_opcode = words.next().expect("javap instruction opcode");
        let (opcode, compact_slot) = raw_opcode
            .rsplit_once('_')
            .filter(|(opcode, slot)| {
                matches!(*opcode, "aload" | "astore") && slot.parse::<u16>().is_ok()
            })
            .map_or((raw_opcode, None), |(opcode, slot)| {
                (
                    opcode,
                    Some(slot.parse::<u16>().expect("compact local slot")),
                )
            });
        if matches!(opcode, "aload" | "astore") {
            local_slots.push(compact_slot.unwrap_or_else(|| {
                words
                    .next()
                    .expect("javap local-slot operand")
                    .parse::<u16>()
                    .expect("numeric javap local slot")
            }));
        }
        instructions.push(opcode.to_string());
        if opcode == "putfield" && line.contains("$create$1.label:I") {
            break;
        }
    }
    (instructions, local_slots)
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

    let (want, reference_slots) = reuse_prefix(&reference);
    assert_eq!(
        want,
        [
            "aload",
            "instanceof",
            "ifeq",
            "aload",
            "checkcast",
            "astore",
            "aload",
            "getfield",
            "ldc",
            "iand",
            "ifeq",
            "aload",
            "dup",
            "getfield",
            "ldc",
            "isub",
            "putfield",
        ],
        "unexpected kotlinc reuse prologue:\n{reference}"
    );
    assert_eq!(reference_slots, [3, 3, 6, 6, 6]);
    let (got, krusty_slots) = reuse_prefix(&krusty);
    assert_eq!(
        got,
        want,
        "continuation reuse prologue must match kotlinc exactly through the label update:\n{krusty}"
    );
    assert_eq!(&krusty_slots[..2], [3, 3]);
    assert!(
        krusty_slots[2..]
            .iter()
            .all(|slot| *slot == krusty_slots[2]),
        "every post-cast reuse must load the one bound continuation local: {krusty_slots:?}\n{krusty}"
    );
}
