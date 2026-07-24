//! LocalVariableTable byte-parity: krusty's local/param debug entries must match kotlinc's
//! exactly, byte-for-byte (the LNT slice already landed, so these shapes have no other known
//! divergence).
//!
//! kotlinc's shape (probed on 2.4.0): entries in SCOPE-CLOSE order — inner-block locals first
//! (start = pc after the initializing store, length = to their block's end), then method-scope
//! locals in DECLARATION order (length = to the method end), then parameters LAST (start 0,
//! length = code length).

use super::common;

fn run(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

#[test]
fn params_only() {
    run(
        "lvtParams",
        "fun onlyParams(x: Long, y: Double): Long {\n    return x\n}\n",
        "LvtParamsKt",
    );
}

#[test]
fn locals_in_declaration_order() {
    run(
        "lvtLocals",
        "fun locals(p: Int): Int {\n    val a = p + 1\n    val b = a + 2\n    return a + b\n}\n",
        "LvtLocalsKt",
    );
}

#[test]
fn nested_scope_local_closes_first() {
    // NOT byte-comparable to kotlinc yet: kotlinc REUSES the dead inner local's slot for `c` and
    // omits the branch fall-through `goto` (both separate, tracked divergences). Pin krusty's OWN
    // shape instead: the block-scoped `inner` must get an LVT entry SHORTER than the method (it
    // dies at its block's close), while `a` runs to the method end.
    let src = "fun mixed(p: Int, q: String): Int {\n\
    val a = p + 1\n\
    var b = a + 2\n\
    if (b > 3) {\n\
        val inner = b + p\n\
        b = inner\n\
    }\n\
    val c = b\n\
    return c\n\
}\n";
    let Some(classes) = common::compile_in_process_metadata_cp(src, "lvtNested", &[]) else {
        panic!("lvtNested: krusty failed to compile");
    };
    let (_, bytes) = classes
        .iter()
        .find(|(n, _)| n == "LvtNestedKt")
        .expect("facade emitted");
    let Some(jh) = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())
    else {
        eprintln!("skip (lvtNested: JAVA_HOME unavailable)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("krusty_lvtn_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("LvtNestedKt.class");
    std::fs::write(&p, bytes).unwrap();
    let out = std::process::Command::new(format!("{jh}/bin/javap"))
        .args(["-c", "-l", "-p"])
        .arg(&p)
        .output()
        .expect("javap runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    let entry = |name: &str| -> (u32, u32) {
        let line = text
            .lines()
            .find(|l| l.split_whitespace().nth(3) == Some(name))
            .unwrap_or_else(|| panic!("LVT entry for {name} missing:\n{text}"));
        let it = l_nums(line);
        (it.0, it.1)
    };
    fn l_nums(l: &str) -> (u32, u32) {
        let mut ws = l.split_whitespace();
        let start = ws.next().unwrap().parse().unwrap();
        let len = ws.next().unwrap().parse().unwrap();
        (start, len)
    }
    let (inner_start, inner_len) = entry("inner");
    let (a_start, a_len) = entry("a");
    assert!(
        inner_len < a_len,
        "scoped 'inner' (start {inner_start}, len {inner_len}) must be shorter than method-scope 'a' (start {a_start}, len {a_len})\n{text}"
    );
    assert!(inner_start > a_start, "'inner' opens after 'a'\n{text}");
}

#[test]
fn guarded_param_fn_full_bytes() {
    // The LNT slice pinned this at javap level (LVT missing); with LVT it must be byte-identical.
    run(
        "lvtGuarded",
        "fun act() {\n}\n\
fun len2(s: String): Int {\n    act()\n    return 5\n}\n",
        "LvtGuardedKt",
    );
}
