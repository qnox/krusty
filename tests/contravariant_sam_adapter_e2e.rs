//! An `in` / `? super` SAM capture is a Kotlin function plus a widened adapter.
//!
//! `Stream.forEach` is `Consumer<? super T>`. kotlinc types `{ action(it) }` as `(String) -> Unit`
//! and adapts that value with `void (Function1, Object)`. An exact `Consumer<String>`, an `out`
//! return, and a star stay one direct SAM method.

use super::common;

const SOURCE: &str = r#"
import java.util.function.Consumer
import java.util.function.Function
import java.util.function.Supplier
import java.util.stream.Stream

fun interface Kin<T> { fun accept(value: T) }

fun takeExact(c: Consumer<String>) {}
fun takeWild(c: Consumer<in String>) {}
fun takeOut(s: Supplier<out String>) {}
fun takeStar(c: Consumer<*>) {}
fun takeInAny(c: Consumer<in Any>) {}
fun takeInAnyQ(c: Consumer<in Any?>) {}
fun takeFn(f: Function<in String, out Int>) {}
fun takeKin(c: Kin<in String>) {}

fun exact(action: (String) -> Unit) { takeExact { action(it) } }
fun wild(action: (String) -> Unit) { takeWild { action(it) } }
fun stream(lines: Stream<String>, action: (String) -> Unit) { lines.forEach { action(it) } }
fun exactValue(action: (String) -> Unit) { takeExact(action) }
fun wildValue(action: (String) -> Unit) { takeWild(action) }
fun supplierOut() { takeOut { "a" } }
fun star(action: (Any?) -> Unit) { takeStar { action(it) } }
fun inAny() { takeInAny { println(it) } }
fun inAnyQ() { takeInAnyQ { println(it) } }
fun useFn() { takeFn { it.length } }
fun kotlinIn(action: (String) -> Unit) { takeKin { action(it) } }
"#;

struct Method {
    header: String,
    descriptor: String,
    body: String,
}

fn strip_offset(line: &str) -> &str {
    match line.find(": ") {
        Some(split)
            if line[..split]
                .chars()
                .all(|character| character.is_ascii_digit()) =>
        {
            &line[split + 2..]
        }
        _ => line,
    }
}

/// Instruction text with constant-pool indexes removed and the operand comment kept, so a
/// metafactory instantiated type and an intrinsic name survive the comparison.
fn instruction(line: &str) -> String {
    let line = strip_offset(line.trim());
    let (code, comment) = line
        .split_once("//")
        .map(|(code, comment)| (code.trim(), Some(comment.trim())))
        .unwrap_or((line, None));
    let opcode = code
        .split_whitespace()
        .filter(|word| !word.starts_with('#') && *word != ",")
        .collect::<Vec<_>>()
        .join(" ");
    let Some(comment) = comment else {
        return opcode;
    };
    let operand = comment
        .strip_prefix("InvokeDynamic")
        .map(|rest| {
            rest.trim()
                .trim_start_matches('#')
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(':')
        })
        .unwrap_or(comment);
    format!("{opcode} {operand}")
}

fn parse_methods(dump: &str) -> Vec<Method> {
    let mut methods = Vec::new();
    let mut current: Option<Method> = None;
    let mut body = Vec::new();
    let flush =
        |methods: &mut Vec<Method>, current: &mut Option<Method>, body: &mut Vec<String>| {
            if let Some(mut method) = current.take() {
                method.body = body.join("\n");
                methods.push(method);
                body.clear();
            }
        };
    for raw in dump.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("Compiled from") || line.starts_with("Code:") {
            continue;
        }
        if line.ends_with(';') && line.contains('(') {
            flush(&mut methods, &mut current, &mut body);
            current = Some(Method {
                header: line.to_string(),
                descriptor: String::new(),
                body: String::new(),
            });
            continue;
        }
        if let Some(method) = current.as_mut() {
            if let Some(descriptor) = line.strip_prefix("descriptor: ") {
                method.descriptor = descriptor.to_string();
                continue;
            }
        }
        if current.is_some() && !line.starts_with('}') {
            let rendered = instruction(line);
            if !rendered.is_empty() {
                body.push(rendered);
            }
        }
    }
    flush(&mut methods, &mut current, &mut body);
    methods
}

fn method_name(header: &str) -> &str {
    header
        .rsplit_once(' ')
        .map(|(_, name)| name.split('(').next().unwrap_or(name))
        .unwrap_or(header)
}

fn compile_both() -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let ours_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&ours_dir).ok()?;
    let source = dir.join("ProjectedSam.kt");
    std::fs::write(&source, SOURCE).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let classpath = [common::stdlib_jar(), common::jdk_modules()];
    let emitted = common::compile_in_process_metadata_cp(SOURCE, "ProjectedSam", &classpath)
        .expect("krusty compiles the projected SAM fixture");
    for (name, bytes) in &emitted {
        let path = ours_dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(&path, bytes).ok()?;
    }
    Some((dir, reference_dir, ours_dir))
}

fn disassembly(root: &std::path::Path, class: &str, verbose: bool) -> String {
    let mut args = vec!["-p", "-c", "-s"];
    if verbose {
        args.push("-v");
    }
    args.extend(["-cp", &root.to_string_lossy(), class]);
    common::javap(&args).unwrap_or_else(|| panic!("javap {class}"))
}

fn bootstrap_contracts(dump: &str) -> Vec<String> {
    let mut contracts = Vec::new();
    let mut arguments = Vec::new();
    let mut collecting = false;
    for line in dump.lines() {
        let line = line.trim();
        if line.starts_with("BootstrapMethods:") {
            collecting = false;
            continue;
        }
        if line.starts_with("Method arguments:") {
            arguments.clear();
            collecting = true;
            continue;
        }
        if collecting && line.starts_with('#') {
            let argument = line
                .split_once(' ')
                .map(|(_, rest)| rest.trim())
                .unwrap_or(line);
            let argument = argument
                .strip_prefix("REF_invokeStatic ")
                .unwrap_or(argument);
            arguments.push(argument.to_string());
            if arguments.len() == 3 {
                contracts.push(arguments.join(" | "));
                arguments.clear();
                collecting = false;
            }
        } else if collecting {
            collecting = false;
        }
    }
    contracts.sort();
    contracts
}

/// Kotlin function literals still null-check a SAM parameter with the pre-existing expression
/// intrinsic. The adapter methods, direct exact/star/out shapes, and call sites are compared in
/// full. A function-value adapter's `p0` check is the same gap.
const FUNCTION_LITERAL_BODIES: &[&str] = &[
    "exact$lambda$0",
    "exactValue$lambda$0",
    "inAny$lambda$0",
    "kotlinIn$lambda$0",
    "stream$lambda$0",
    "useFn$lambda$0",
    "wild$lambda$0",
];

fn assert_methods(reference: &[Method], ours: &[Method], allow_body_gap: &[&str]) {
    let reference_names: Vec<_> = reference
        .iter()
        .map(|method| method_name(&method.header))
        .collect();
    let our_names: Vec<_> = ours
        .iter()
        .map(|method| method_name(&method.header))
        .collect();
    assert_eq!(our_names, reference_names, "method inventory");
    let mut gaps = Vec::new();
    for (reference, ours) in reference.iter().zip(ours) {
        let name = method_name(&reference.header);
        assert_eq!(ours.header, reference.header, "{name} declaration");
        assert_eq!(ours.descriptor, reference.descriptor, "{name} descriptor");
        if ours.body != reference.body {
            gaps.push(format!(
                "{name}\n--- kotlinc\n{}\n--- krusty\n{}",
                reference.body, ours.body
            ));
            assert!(
                allow_body_gap.contains(&name),
                "unexpected body difference in {name}:\n{}",
                gaps.last().unwrap()
            );
        } else {
            assert!(
                !allow_body_gap.contains(&name),
                "{name} now matches kotlinc; drop it from the function-literal gap list"
            );
        }
    }
    let gap_names: Vec<_> = gaps
        .iter()
        .map(|gap| gap.split('\n').next().unwrap())
        .collect();
    let mut expected = allow_body_gap.to_vec();
    expected.sort_unstable();
    let mut actual = gap_names.clone();
    actual.sort();
    assert_eq!(
        actual,
        expected,
        "function-literal body gaps:\n{}",
        gaps.join("\n")
    );
}

#[test]
fn an_in_projected_sam_adapts_a_unit_function() {
    let Some((dir, reference_dir, ours_dir)) = compile_both() else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let reference = parse_methods(&disassembly(&reference_dir, "ProjectedSamKt", false));
    let ours = parse_methods(&disassembly(&ours_dir, "ProjectedSamKt", false));
    assert_methods(&reference, &ours, FUNCTION_LITERAL_BODIES);

    let reference_wrapper = parse_methods(&disassembly(
        &reference_dir,
        "ProjectedSamKt$sam$Kin$0",
        false,
    ));
    let our_wrapper = parse_methods(&disassembly(&ours_dir, "ProjectedSamKt$sam$Kin$0", false));
    assert_methods(&reference_wrapper, &our_wrapper, &[]);

    let reference_boot = bootstrap_contracts(&disassembly(&reference_dir, "ProjectedSamKt", true));
    let our_boot = bootstrap_contracts(&disassembly(&ours_dir, "ProjectedSamKt", true));
    assert_eq!(
        our_boot, reference_boot,
        "metafactory sam type, implementation, and instantiated type"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_projected_stream_runs_the_action() {
    const SRC: &str = "import java.util.stream.Stream\n\
        fun box(): String {\n\
            val out = StringBuilder()\n\
            Stream.of(\"O\", \"K\").forEach { out.append(it) }\n\
            return out.toString()\n\
        }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "ProjectedSamRun")
            .expect("projected forEach runs"),
        "OK"
    );
}
