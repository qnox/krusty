use super::common;

fn debug_metadata(bytes: &[u8], file_name: &str) -> String {
    let dir = common::scratch_dir().expect("scratch dir");
    let class_file = dir.join(file_name);
    std::fs::write(&class_file, bytes).expect("write class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("pooled JavaRunner unavailable");
    let _ = std::fs::remove_file(&class_file);
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with("nl=["))
        .unwrap_or_else(|| panic!("no debug-metadata resume lines in:\n{text}"))
        .to_string()
}

fn kotlinc_class(source_name: &str, source: &str, class_name: &str) -> Option<Vec<u8>> {
    let root = common::scratch_dir().expect("reference scratch dir");
    let out = root.join("classes");
    std::fs::create_dir_all(&out).expect("reference output dir");
    let source_path = root.join(format!("{source_name}.kt"));
    std::fs::write(&source_path, source).expect("reference source");
    let compiled = common::kotlinc_compile(&[
        "-d".to_string(),
        out.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ]);
    let Some((code, stderr)) = compiled else {
        let _ = std::fs::remove_dir_all(root);
        return None;
    };
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let bytes = std::fs::read(out.join(format!("{class_name}.class")))
        .unwrap_or_else(|error| panic!("read kotlinc class {class_name}: {error}"));
    let _ = std::fs::remove_dir_all(root);
    Some(bytes)
}

fn assert_resume_lines(name: &str, source: &str, class_name: &str) {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let Some(reference) = kotlinc_class(name, source, class_name) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    let classes = common::compile_in_process_files(
        &[(name, source)],
        &[stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("compile when-branch continuation");
    let simple = class_name.rsplit('/').next().expect("simple class name");
    let ours = classes
        .iter()
        .find_map(|(cls, bytes)| (cls == class_name).then_some(bytes))
        .unwrap_or_else(|| panic!("{class_name} not emitted"));
    assert_eq!(
        debug_metadata(ours, &format!("ours-{simple}.class")),
        debug_metadata(&reference, &format!("ref-{simple}.class")),
        "{name}: continuation resume lines must match kotlinc"
    );
}

/// A suspension inside a `when` BRANCH resumes on the line of the NEXT branch — the code the arm
/// falls into — not on the line following the `when` itself. krusty used the enclosing statement's
/// successor line, which reads one line too far whenever the suspending arm is not the last.
#[test]
fn a_when_branch_resumes_on_the_next_branch_line() {
    assert_resume_lines(
        "BranchResume",
        "package demo\n\
        class Store {\n\
        \x20 suspend fun find(name: String): String? = name.takeIf { it.isNotEmpty() }\n\
        }\n\
        class Arms(private val store: Store) {\n\
        \x20 suspend fun pick(id: String, retry: Boolean): String {\n\
        \x20\x20 val fallback = \"a\"\n\
        \x20\x20 val found = store.find(id)\n\
        \x20\x20 return when {\n\
        \x20\x20\x20 found != null -> found\n\
        \x20\x20\x20 retry -> store.find(id) ?: fallback\n\
        \x20\x20\x20 else -> fallback\n\
        \x20\x20 }\n\
        \x20 }\n\
        }\n",
        "demo/Arms$pick$1",
    );
}

/// The next branch's CONDITION is what the arm falls into when one follows.
#[test]
fn a_when_branch_resumes_on_the_next_condition_line() {
    assert_resume_lines(
        "ConditionResume",
        "package demo\n\
        class Store {\n\
        \x20 suspend fun find(name: String): String? = name.takeIf { it.isNotEmpty() }\n\
        }\n\
        class Arms(private val store: Store) {\n\
        \x20 suspend fun pick(id: String, retry: Boolean): String {\n\
        \x20\x20 val fallback = \"a\"\n\
        \x20\x20 return when {\n\
        \x20\x20\x20 retry -> store.find(id) ?: fallback\n\
        \n\
        \x20\x20\x20 id.isEmpty() -> fallback\n\
        \x20\x20\x20 else -> fallback\n\
        \x20\x20 }\n\
        \x20 }\n\
        }\n",
        "demo/Arms$pick$1",
    );
}

/// Block-bodied arms behave the same: the then-arm falls into the else arm's first executable line.
#[test]
fn a_block_arm_resumes_on_the_next_arm_body_line() {
    assert_resume_lines(
        "BlockArmResume",
        "package demo\n\
        class Leafs {\n\
        \x20 suspend fun leaf(v: Int): Int = v\n\
        }\n\
        class Chooser(private val leafs: Leafs) {\n\
        \x20 suspend fun choose(flag: Boolean): Int {\n\
        \x20\x20 val base = 1\n\
        \x20\x20 val picked = if (flag) {\n\
        \x20\x20\x20 leafs.leaf(1)\n\
        \x20\x20 } else {\n\
        \x20\x20\x20 leafs.leaf(2)\n\
        \x20\x20 }\n\
        \x20\x20 return picked + base\n\
        \x20 }\n\
        }\n",
        "demo/Chooser$choose$1",
    );
}

/// The LAST arm has no next branch: it falls into the `when`'s merge, which kotlinc attributes to the
/// `when` expression's own line — not to the statement that follows the `when`.
#[test]
fn a_last_arm_resumes_on_the_when_line() {
    assert_resume_lines(
        "LastArmResume",
        "package demo\n\
        class Leafs {\n\
        \x20 suspend fun leaf(v: Int): Int = v\n\
        }\n\
        class Last(private val leafs: Leafs) {\n\
        \x20 suspend fun choose(flag: Boolean): Int {\n\
        \x20\x20 val base = 1\n\
        \x20\x20 val picked = when {\n\
        \x20\x20\x20 flag -> base\n\
        \x20\x20\x20 else -> leafs.leaf(2)\n\
        \x20\x20 }\n\
        \x20\x20 return picked + base\n\
        \x20 }\n\
        }\n",
        "demo/Last$choose$1",
    );
}
