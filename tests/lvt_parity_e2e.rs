//! LocalVariableTable parity with kotlinc 2.4.0.

use super::common;

fn run(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

fn javap_krusty(name: &str, src: &str, class: &str) -> Option<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process(src, name, &[stdlib], Some(jdk.as_path()))
        .unwrap_or_else(|| panic!("{name}: krusty failed to compile"));
    let (_, bytes) = classes
        .iter()
        .find(|(emitted, _)| emitted == class)
        .unwrap_or_else(|| panic!("{class} was not emitted"));
    let java_home = std::env::var("KRUSTY_REF_JAVA_HOME")
        .ok()
        .or_else(|| std::env::var("JAVA_HOME").ok())?;
    let dir = std::env::temp_dir().join(format!("krusty_lvt_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let class_file = dir.join(format!("{class}.class"));
    std::fs::write(&class_file, bytes).unwrap();
    let out = std::process::Command::new(format!("{java_home}/bin/javap"))
        .args(["-c", "-l", "-p"])
        .arg(&class_file)
        .output()
        .expect("javap runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "javap failed for {class}");
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn assert_lvt_entry(text: &str, name: &str, descriptor: &str) {
    let found = text.lines().any(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        fields.len() == 5
            && fields[0].parse::<u32>().is_ok()
            && fields[1].parse::<u32>().is_ok()
            && fields[2].parse::<u16>().is_ok()
            && fields[3] == name
            && fields[4] == descriptor
    });
    assert!(found, "LVT entry for {name}: {descriptor} missing:\n{text}");
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
fn instance_receiver_and_parameters_are_recorded() {
    let src = "class Worker {\n\
    fun work(input: Long): Long {\n\
        val result = input + 1\n\
        return result\n\
    }\n\
}\n";
    let Some(text) = javap_krusty("lvtInstance", src, "Worker") else {
        eprintln!("skip (lvtInstance: JAVA_HOME unavailable)");
        return;
    };
    for (name, descriptor) in [("result", "J"), ("this", "LWorker;"), ("input", "J")] {
        assert_lvt_entry(&text, name, descriptor);
    }
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
    let Some(text) = javap_krusty("lvtNested", src, "LvtNestedKt") else {
        eprintln!("skip (lvtNested: JAVA_HOME unavailable)");
        return;
    };
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
    run(
        "lvtGuarded",
        "fun act() {\n}\n\
fun len2(s: String): Int {\n    act()\n    return 5\n}\n",
        "LvtGuardedKt",
    );
}

#[test]
fn source_local_forms_are_recorded() {
    let src = r#"
fun loop(xs: IntArray): Int {
    var sum = 0
    for (item in xs) {
        sum += item
    }
    return sum
}

fun rangeLoop(): Int {
    var total = 0
    for (index in 0..2) {
        total += index
    }
    return total
}

fun unitLoop(values: List<Unit>): Int {
    var total = 0
    for (value in values) {
        total++
    }
    return total
}

data class Pairish(val first: Int, val second: String)

fun destructure(pair: Pairish): Int {
    val (number, text) = pair
    return number + text.length
}

fun late(): String {
    lateinit var text: String
    text = "OK"
    return text
}

fun captured(): Int {
    var value = 1
    val read = { value }
    value = 2
    return read()
}

class Delegate {
    operator fun getValue(thisRef: Any?, property: Any?): Int = 1
}

fun delegated(): Int {
    val value: Int by Delegate()
    return value
}
"#;
    let Some(text) = javap_krusty("lvtSourceForms", src, "LvtSourceFormsKt") else {
        eprintln!("skip (lvtSourceForms: JAVA_HOME unavailable)");
        return;
    };

    for (name, descriptor) in [
        ("sum", "I"),
        ("item", "I"),
        ("index", "I"),
        ("value", "Lkotlin/Unit;"),
        ("number", "I"),
        ("text", "Ljava/lang/String;"),
        ("value", "Lkotlin/jvm/internal/Ref$IntRef;"),
        ("read", "Lkotlin/jvm/functions/Function0;"),
        ("value$delegate", "LDelegate;"),
    ] {
        assert_lvt_entry(&text, name, descriptor);
    }
}

#[test]
fn catch_parameter_is_recorded() {
    let src = "fun caught(): Int {\n\
    try {\n\
        return 1\n\
    } catch (failure: Exception) {\n\
        return 2\n\
    }\n\
}\n";
    let Some(text) = javap_krusty("lvtCatch", src, "LvtCatchKt") else {
        eprintln!("skip (lvtCatch: JAVA_HOME unavailable)");
        return;
    };
    assert_lvt_entry(&text, "failure", "Ljava/lang/Exception;");
}
