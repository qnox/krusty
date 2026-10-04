//! An unintercepted suspension block reports a suspension to the debug probes, as kotlinc does.
//!
//! kotlinc follows the value of a `suspendCoroutineUninterceptedOrReturn` block with
//! `dup; getCOROUTINE_SUSPENDED; if_acmpne; aload <continuation>; probeCoroutineSuspended`, the
//! continuation being `$completion` in a function without a state machine and the machine's own
//! continuation (cast to `Continuation`) in one with it. krusty returned the value unchecked.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_block, method_instructions};

const SOURCE: &str = "import kotlin.coroutines.*\n\
    import kotlin.coroutines.intrinsics.*\n\
    class ProbePayload(val text: String)\n\
    suspend fun direct(): ProbePayload = suspendCoroutineUninterceptedOrReturn { x ->\n\
    \x20   x.resume(ProbePayload(\"OK\"))\n\
    \x20   COROUTINE_SUSPENDED\n\
    }\n\
    suspend fun machine(): ProbePayload {\n\
    \x20   val first = direct()\n\
    \x20   val second = suspendCoroutineUninterceptedOrReturn<ProbePayload> { x -> x.resume(first); COROUTINE_SUSPENDED }\n\
    \x20   return ProbePayload(first.text + second.text)\n\
    }\n";

/// The slot an `aload` reads, for both its short and its operand form.
fn loaded_slot(code: &str) -> Option<&str> {
    let (op, operand) = code.split_once(' ').unwrap_or((code, ""));
    match op.strip_prefix("aload_") {
        Some(slot) => Some(slot),
        None if op == "aload" => Some(operand.trim()),
        None => None,
    }
}

/// Each probe window of a method body: from the `dup` of a block's value through its
/// `probeCoroutineSuspended` call, without offsets or branch targets. The two compilers place a
/// state machine's own locals differently, so a load of the machine's continuation (the local
/// whose `result` the machine reads) is named rather than numbered.
fn probe_windows(body: &[String]) -> Vec<Vec<String>> {
    let codes: Vec<&str> = body
        .iter()
        .map(|row| row.split_once(": ").map_or(row.as_str(), |(_, code)| code))
        .collect();
    let machine = codes.windows(2).find_map(|pair| {
        let reads_result =
            pair[1].starts_with("getfield") && pair[1].ends_with(".result:Ljava/lang/Object;");
        reads_result.then(|| loaded_slot(pair[0])).flatten()
    });
    let instructions: Vec<String> = codes
        .iter()
        .map(|code| {
            let op = code.split(' ').next().unwrap_or(code);
            if op.starts_with("if") || op == "goto" {
                op.to_string()
            } else if machine.is_some() && loaded_slot(code) == machine {
                "aload <machine continuation>".to_string()
            } else {
                code.to_string()
            }
        })
        .collect();
    let mut windows = Vec::new();
    for (start, instruction) in instructions.iter().enumerate() {
        let checks_suspension = instructions
            .get(start + 1)
            .is_some_and(|next| next.contains("getCOROUTINE_SUSPENDED"));
        if instruction != "dup" || !checks_suspension {
            continue;
        }
        if let Some(end) = instructions[start..]
            .iter()
            .position(|insn| insn.contains("probeCoroutineSuspended"))
        {
            windows.push(instructions[start..=start + end].to_vec());
        }
    }
    windows
}

/// The exact bytecode offset and active source line at each intrinsic probe window.
fn probe_source_positions(disassembly: &str, header: &str) -> Vec<(u32, u32)> {
    let block = method_block(disassembly, header);
    let lines = block
        .iter()
        .skip_while(|line| line.as_str() != "LineNumberTable:")
        .skip(1)
        .take_while(|line| line.starts_with("line "))
        .map(|line| {
            let (source, offset) = line
                .strip_prefix("line ")
                .and_then(|line| line.split_once(": "))
                .expect("a javap line-table row");
            (
                offset.parse::<u32>().expect("a bytecode offset"),
                source.parse::<u32>().expect("a source line"),
            )
        })
        .collect::<Vec<_>>();
    method_instructions(disassembly, header)
        .windows(2)
        .filter(|pair| pair[0].ends_with(": dup") && pair[1].contains("getCOROUTINE_SUSPENDED"))
        .map(|pair| {
            let offset = pair[0]
                .split_once(':')
                .and_then(|(offset, _)| offset.parse::<u32>().ok())
                .expect("an instruction offset");
            let line = lines
                .iter()
                .rev()
                .find(|(start, _)| *start <= offset)
                .map(|(_, line)| *line)
                .expect("a source line for the probe window");
            (offset, line)
        })
        .collect()
}

#[test]
fn unintercepted_blocks_probe_their_suspension_like_kotlinc() {
    let built = compare_with_kotlinc_plugin(
        "IntrinsicProbes",
        SOURCE,
        "IntrinsicProbesKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc and javap are required");
    for member in ["java.lang.Object direct(", "java.lang.Object machine("] {
        let reference = probe_windows(&method_instructions(&built.reference, member));
        assert!(!reference.is_empty(), "kotlinc: no probe in {member}");
        assert_eq!(
            probe_windows(&method_instructions(&built.krusty, member)),
            reference,
            "{member}"
        );
    }
    let direct = "public static final java.lang.Object direct(kotlin.coroutines.Continuation<? super ProbePayload>);";
    let reference_positions = probe_source_positions(&built.reference, direct);
    assert_eq!(
        reference_positions,
        [(29, 4)],
        "kotlinc: direct probe bytecode offset and source line"
    );
    assert_eq!(
        probe_source_positions(&built.krusty, direct),
        reference_positions,
        "direct: exact probe bytecode offset and source line"
    );
}

#[test]
fn a_probed_block_still_resumes() {
    let main = format!(
        "{SOURCE}\
         fun box(): String {{\n\
         \x20   var result: ProbePayload? = null\n\
         \x20   ::machine.startCoroutine(Continuation(EmptyCoroutineContext) {{ result = it.getOrThrow() }})\n\
         \x20   return if (result?.text == \"OKOK\") \"OK\" else \"fail\"\n\
         }}\n"
    );
    common::expect_box_same_as_kotlinc(&main, "IntrinsicProbesRun");
}

/// A method body from its first probe window to its end, without offsets or branch targets.
fn from_first_probe(body: &[String]) -> Vec<String> {
    let codes: Vec<&str> = body
        .iter()
        .map(|row| row.split_once(": ").map_or(row.as_str(), |(_, code)| code))
        .collect();
    let start = codes
        .windows(2)
        .position(|pair| pair[0] == "dup" && pair[1].contains("getCOROUTINE_SUSPENDED"))
        .expect("a probe window");
    codes[start..]
        .iter()
        .map(|code| {
            let op = code.split(' ').next().unwrap_or(code);
            match op.starts_with("if") || op == "goto" {
                true => op.to_string(),
                false => code.to_string(),
            }
        })
        .collect()
}

#[test]
fn a_unit_block_statement_returns_its_suspension_like_kotlinc() {
    // A `Unit` function whose body is the block: kotlinc probes the block's value, returns it when
    // it is `COROUTINE_SUSPENDED`, and only then answers `Unit`, with no state machine.
    let source = "import kotlin.coroutines.*\n\
        import kotlin.coroutines.intrinsics.*\n\
        class ProbeRecord { var text = \"fail\" }\n\
        suspend fun record(target: ProbeRecord): Unit = suspendCoroutineUninterceptedOrReturn { x ->\n\
        \x20   target.text = \"OK\"\n\
        \x20   x.resume(Unit)\n\
        \x20   COROUTINE_SUSPENDED\n\
        }\n";
    let built = compare_with_kotlinc_plugin(
        "IntrinsicUnitProbe",
        source,
        "IntrinsicUnitProbeKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc and javap are required");
    let member = "java.lang.Object record(";
    let reference = from_first_probe(&method_instructions(&built.reference, member));
    assert_eq!(
        reference.last().map(String::as_str),
        Some("areturn"),
        "kotlinc: {reference:?}"
    );
    assert_eq!(
        from_first_probe(&method_instructions(&built.krusty, member)),
        reference
    );
}
