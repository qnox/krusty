//! The order a continuation class lays out its spill fields in.
//!
//! kotlinc groups them by kind — references `L$n`, ints `I$n`, longs `J$n` — and orders the GROUPS
//! by the kind that is spilled first, so the layout follows the code. krusty used one fixed order
//! for every method, which matched only when the code happened to agree with it.
//!
//! One `$N` class exists per suspend call site, and the generated HTTP clients in the corpus are
//! almost entirely suspend functions, so this is one of the most repeated differences there is.
use super::common;

/// An `Int` local reaches the suspension first, so the int group comes first.
#[test]
fn an_int_spilled_first_lays_out_its_group_first() {
    let src = "suspend fun fetch(a: Int): String = \"\"\n\
               \n\
               suspend fun run(x: Int, y: String, z: Long): String {\n\
               \x20   val p = x + 1\n\
               \x20   val q = y + \"!\"\n\
               \x20   val r = z * 2\n\
               \x20   val first = fetch(p)\n\
               \x20   return first + q + r\n\
               }\n";
    assert_eq!(
        spill_fields(src, "IntFirst", "IntFirstKt$run$1"),
        ["I$0", "I$1", "L$0", "L$1", "J$0", "J$1"],
    );
}

/// The same locals in another order lay the groups out in that order — the point being that there
/// is no fixed table, which a single fixture could not show.
#[test]
fn a_reference_spilled_first_lays_out_its_group_first() {
    let src = "suspend fun fetch(a: Int): String = \"\"\n\
               \n\
               suspend fun order(s: String, n: Long, k: Int): String {\n\
               \x20   val a = s + \"!\"\n\
               \x20   val b = n * 2\n\
               \x20   val c = k + 1\n\
               \x20   val got = fetch(c)\n\
               \x20   return got + a + b\n\
               }\n";
    assert_eq!(
        spill_fields(src, "RefFirst", "RefFirstKt$order$1"),
        ["L$0", "L$1", "J$0", "J$1", "I$0", "I$1"],
    );
}

/// The spill fields of one continuation class, in layout order, from BOTH compilers — asserted
/// equal, and returned so the test can also state what they are.
fn spill_fields(src: &str, name: &str, class: &str) -> Vec<String> {
    let Some((reference, krusty)) = disassemble_both(name, src, class) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return Vec::new();
    };
    let fields = |text: &str| {
        text.lines()
            .map(str::trim)
            .filter_map(|line| line.split_whitespace().last())
            .filter_map(|last| last.strip_suffix(';'))
            .filter(|name| {
                let mut parts = name.split('$');
                matches!(parts.next(), Some("L" | "I" | "J"))
                    && parts.next().is_some_and(|n| n.parse::<u32>().is_ok())
            })
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let want = fields(&reference);
    assert!(!want.is_empty(), "{class}: kotlinc spills something");
    assert_eq!(fields(&krusty), want, "{class} spill field layout");
    want
}

/// The same source through both compilers, disassembled: `(kotlinc, krusty)`.
fn disassemble_both(name: &str, src: &str, class: &str) -> Option<(String, String)> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let krusty_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&krusty_dir).ok()?;
    let source = dir.join(format!("{name}.kt"));
    std::fs::write(&source, src).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "25".to_string(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "{name}: kotlinc failed: {stderr}");
    let classes = common::compile_in_process(src, name, &[common::stdlib_jar()], None)
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    for (internal, bytes) in &classes {
        let path = krusty_dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(path, bytes).ok()?;
    }
    let reference = common::javap(&["-p", "-cp", &reference_dir.to_string_lossy(), class])?;
    let krusty = common::javap(&["-p", "-cp", &krusty_dir.to_string_lossy(), class])?;
    let _ = std::fs::remove_dir_all(dir);
    Some((reference, krusty))
}
