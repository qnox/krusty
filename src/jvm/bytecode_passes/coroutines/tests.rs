//! The transformation against kotlinc 2.4.20's own output. Each test builds the body kotlinc's
//! codegen hands `CoroutineTransformerMethodVisitor` — suspend calls between `InlineMarker` calls —
//! transforms it, and compares the result with what kotlinc wrote for the same source: the
//! instructions (after the `nop` removal and dead-code elimination that run later in both
//! compilers), the line numbers and the local-variable table, each by instruction position. Where
//! kotlinc's optimizer changes the transformed method further, the expected listing is the
//! transformer's own output, dumped from kotlinc.

use super::super::opcodes::*;
use super::markers::{inline_call_marker, mark, SuspendMarker};
use super::{
    transform_named_function, transform_suspend_lambda, CoroutineError, DeclaredSpillFields,
    NamedFunction, SpillField, StateMachine, SuspendLambda, Transformed,
};
use crate::jvm::method_node::{Constant, Insn, LabelId, LocalVariable, MethodNode, Node};

/// Builds a body node by node.
struct Body {
    method: MethodNode,
}

impl Body {
    fn new(access: u16, name: &str, desc: &str, max_locals: u16) -> Body {
        let mut method = MethodNode::new(access, name, desc);
        method.max_locals = max_locals;
        Body { method }
    }

    fn label(&mut self) -> LabelId {
        let label = self.method.new_label();
        self.method.nodes.push(Node::Label(label));
        label
    }

    fn line(&mut self, line: u16) -> LabelId {
        let label = self.label();
        self.method.nodes.push(Node::Line { line, start: label });
        label
    }

    fn insn(&mut self, insn: Insn) -> &mut Self {
        self.method.nodes.push(Node::Insn(insn));
        self
    }

    fn op(&mut self, op: u8) -> &mut Self {
        self.insn(Insn::Op(op))
    }

    fn var(&mut self, op: u8, slot: u16) -> &mut Self {
        self.insn(Insn::Var { op, slot })
    }

    fn call(&mut self, op: u8, owner: &str, name: &str, desc: &str) -> &mut Self {
        self.insn(Insn::Method {
            op,
            owner: owner.to_string(),
            name: name.to_string(),
            desc: desc.to_string(),
            interface: false,
        })
    }

    fn nodes(&mut self, nodes: Vec<Node>) -> &mut Self {
        self.method.nodes.extend(nodes);
        self
    }

    /// kotlinc's shape of a non-inline suspend call whose arguments `arguments` pushes.
    fn suspend_call(
        &mut self,
        arguments: impl FnOnce(&mut Body),
        unit: bool,
        owner: &str,
        name: &str,
        desc: &str,
    ) -> &mut Self {
        self.nodes(vec![inline_call_marker(true)]);
        arguments(self);
        self.nodes(mark(SuspendMarker::BeforeSuspend));
        if unit {
            self.nodes(mark(SuspendMarker::BeforeSuspendUnitCall));
        }
        self.call(INVOKESTATIC, owner, name, desc);
        self.nodes(mark(SuspendMarker::AfterSuspend));
        self.nodes(vec![inline_call_marker(false)])
    }

    fn local(&mut self, name: &str, desc: &str, start: LabelId, end: LabelId, slot: u16) {
        self.method.local_variables.push(LocalVariable {
            name: name.to_string(),
            desc: desc.to_string(),
            start,
            end,
            slot,
        });
    }
}

fn mnemonic(op: u8) -> String {
    let name = match op {
        NOP => "nop",
        ACONST_NULL => "aconst_null",
        ICONST_M1..=ICONST_5 => return format!("iconst_{}", i32::from(op) - i32::from(ICONST_0)),
        ILOAD => "iload",
        ALOAD => "aload",
        ISTORE => "istore",
        ASTORE => "astore",
        POP => "pop",
        DUP => "dup",
        IADD => "iadd",
        ISUB => "isub",
        IAND => "iand",
        IFEQ => "ifeq",
        IF_ACMPNE => "if_acmpne",
        GOTO => "goto",
        ARETURN => "areturn",
        RETURN => "return",
        GETSTATIC => "getstatic",
        GETFIELD => "getfield",
        PUTFIELD => "putfield",
        INVOKEVIRTUAL => "invokevirtual",
        INVOKESPECIAL => "invokespecial",
        INVOKESTATIC => "invokestatic",
        NEW => "new",
        CHECKCAST => "checkcast",
        INSTANCEOF => "instanceof",
        ATHROW => "athrow",
        BIPUSH => "bipush",
        _ => return format!("op_{op:#04x}"),
    };
    name.to_string()
}

fn render(insn: &Insn, index_of: &dyn Fn(LabelId) -> usize) -> String {
    match insn {
        Insn::Op(op) => mnemonic(*op),
        Insn::Int { op, operand } => format!("{} {operand}", mnemonic(*op)),
        Insn::Var { op, slot } => format!("{} {slot}", mnemonic(*op)),
        Insn::Type { op, class } => format!("{} {class}", mnemonic(*op)),
        Insn::Field {
            op,
            owner,
            name,
            desc,
        } => format!("{} {owner}.{name}:{desc}", mnemonic(*op)),
        Insn::Method {
            op,
            owner,
            name,
            desc,
            ..
        } => {
            format!("{} {owner}.{name}{desc}", mnemonic(*op))
        }
        Insn::Jump { op, target } => format!("{} @{}", mnemonic(*op), index_of(*target)),
        Insn::Ldc(Constant::Int(value)) => format!("ldc {value}"),
        Insn::Ldc(Constant::String(value)) => format!("ldc \"{}\"", value.to_lossy()),
        Insn::TableSwitch {
            low,
            high,
            default,
            labels,
        } => {
            let targets: Vec<String> = labels
                .iter()
                .map(|l| format!("@{}", index_of(*l)))
                .collect();
            format!(
                "tableswitch {low}..{high} [{}] default @{}",
                targets.join(", "),
                index_of(*default)
            )
        }
        other => format!("{other:?}"),
    }
}

/// The method as kotlinc's later passes leave it — no `nop`, no `return` after an `athrow` — listed
/// one instruction per line, then its line numbers and local variables by instruction position.
fn listing(method: &MethodNode) -> String {
    let mut kept = Vec::new();
    let mut previous = None;
    for node in &method.nodes {
        match node {
            Node::Insn(Insn::Op(NOP)) => continue,
            Node::Insn(Insn::Op(RETURN)) if previous == Some(ATHROW) => continue,
            Node::Insn(insn) => {
                previous = Some(super::super::analysis::opcode(insn));
                kept.push(node);
            }
            _ => kept.push(node),
        }
    }
    // A label stands at the position of the next instruction.
    let mut positions = std::collections::HashMap::new();
    let mut position = 0;
    for node in &kept {
        match node {
            Node::Label(label) => {
                positions.insert(*label, position);
            }
            Node::Insn(_) => position += 1,
            Node::Line { .. } => {}
        }
    }
    let index_of = |label: LabelId| positions[&label];
    let mut out = String::new();
    let mut position = 0;
    for node in &kept {
        if let Node::Insn(insn) = node {
            out.push_str(&format!("{position}: {}\n", render(insn, &index_of)));
            position += 1;
        }
    }
    for node in &kept {
        if let Node::Line { line, start } = node {
            out.push_str(&format!("line {line}: {}\n", index_of(*start)));
        }
    }
    for local in &method.local_variables {
        out.push_str(&format!(
            "local {} {} {}..{} slot {}\n",
            local.name,
            local.desc,
            index_of(local.start),
            index_of(local.end),
            local.slot
        ));
    }
    out
}

const STRING: &str = "java/lang/String";
const STRING_BUILDER: &str = "java/lang/StringBuilder";

/// ```kotlin
/// suspend fun one(): Int = 1
/// suspend fun unit() {}
/// suspend fun f(s: String): Int {
///     val a = s.length
///     val dead = "x" + s
///     println(dead)
///     val b = one()
///     unit()
///     return a + b
/// }
/// ```
fn two_suspension_points() -> MethodNode {
    let mut body = Body::new(
        0x0019,
        "f",
        "(Ljava/lang/String;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        5,
    );
    let start = body.line(4);
    body.var(ALOAD, 0)
        .call(INVOKEVIRTUAL, STRING, "length", "()I")
        .var(ISTORE, 2);
    let a_start = body.line(5);
    body.insn(Insn::Type {
        op: NEW,
        class: STRING_BUILDER.to_string(),
    })
    .op(DUP)
    .call(INVOKESPECIAL, STRING_BUILDER, "<init>", "()V")
    .insn(Insn::Int {
        op: BIPUSH,
        operand: 120,
    })
    .call(
        INVOKEVIRTUAL,
        STRING_BUILDER,
        "append",
        "(C)Ljava/lang/StringBuilder;",
    )
    .var(ALOAD, 0)
    .call(
        INVOKEVIRTUAL,
        STRING_BUILDER,
        "append",
        "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
    )
    .call(
        INVOKEVIRTUAL,
        STRING_BUILDER,
        "toString",
        "()Ljava/lang/String;",
    )
    .var(ASTORE, 3);
    let dead_start = body.line(6);
    body.insn(Insn::Field {
        op: GETSTATIC,
        owner: "java/lang/System".to_string(),
        name: "out".to_string(),
        desc: "Ljava/io/PrintStream;".to_string(),
    })
    .var(ALOAD, 3)
    .call(
        INVOKEVIRTUAL,
        "java/io/PrintStream",
        "println",
        "(Ljava/lang/Object;)V",
    );
    body.line(7);
    body.suspend_call(
        |body| {
            body.var(ALOAD, 1);
        },
        false,
        "AKt",
        "one",
        "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
    )
    .insn(Insn::Type {
        op: CHECKCAST,
        class: "java/lang/Number".to_string(),
    })
    .call(INVOKEVIRTUAL, "java/lang/Number", "intValue", "()I")
    .var(ISTORE, 4);
    let b_start = body.line(8);
    body.suspend_call(
        |body| {
            body.var(ALOAD, 1);
        },
        true,
        "AKt",
        "unit",
        "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
    )
    .op(POP);
    body.line(9);
    body.var(ILOAD, 2)
        .var(ILOAD, 4)
        .op(IADD)
        .call(
            INVOKESTATIC,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        .op(ARETURN);
    let end = body.label();
    body.local("a", "I", a_start, end, 2);
    body.local("dead", "Ljava/lang/String;", dead_start, end, 3);
    body.local("b", "I", b_start, end, 4);
    body.local("s", "Ljava/lang/String;", start, end, 0);
    body.local(
        "$completion",
        "Lkotlin/coroutines/Continuation;",
        start,
        end,
        1,
    );
    body.method
}

const F: NamedFunction = NamedFunction {
    owner: "AKt",
    continuation_class: "AKt$f$1",
    source_file: "A.kt",
    line_number: 3,
    completion_slot: 1,
    dispatch_receiver: None,
};

#[test]
fn a_function_with_two_suspension_points_matches_kotlinc() {
    let Transformed::StateMachine(machine) =
        transform_named_function(two_suspension_points(), &F).expect("the body transforms")
    else {
        panic!("a body with non-tail suspension points needs a state machine");
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = *machine;
    assert!(
        method.max_stack > 0,
        "the transformed final body owns its computed max_stack"
    );
    // kotlinc 2.4.20's `AKt.f` (`javap -c -l`, offsets turned into instruction positions).
    assert_eq!(
        listing(&method),
        include_str!("testdata/two_suspension_points.txt")
    );
    let field = |name: &str, descriptor: &str| SpillField {
        name: name.to_string(),
        descriptor: descriptor.to_string(),
    };
    assert_eq!(
        layout.fields,
        vec![
            field("L$0", "Ljava/lang/Object;"),
            field("L$1", "Ljava/lang/Object;"),
            field("I$0", "I"),
            field("I$1", "I"),
        ]
    );
    assert_eq!(debug_metadata.line_numbers, vec![7, 8]);
    assert_eq!(debug_metadata.next_line_numbers, vec![8, 9]);
    assert_eq!(debug_metadata.index_to_label, vec![0, 0, 0, 1, 1, 1, 1]);
    assert_eq!(
        debug_metadata.spilled,
        vec!["L$0", "L$1", "I$0", "L$0", "L$1", "I$0", "I$1"]
    );
    assert_eq!(
        debug_metadata.local_names,
        vec!["s", "dead", "a", "s", "dead", "a", "b"]
    );
    assert_eq!(debug_metadata.class_name, "AKt");
}

const ONE: &str = "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;";

const B: NamedFunction = NamedFunction {
    owner: "BKt",
    continuation_class: "BKt$stack$1",
    source_file: "B.kt",
    line_number: 3,
    completion_slot: 1,
    dispatch_receiver: None,
};

/// ```kotlin
/// suspend fun one(v: Int): Int = v + 1
/// suspend fun tail(v: Int): Int = one(v)
/// ```
#[test]
fn a_tail_call_needs_no_state_machine() {
    let mut body = Body::new(
        0x0019,
        "tail",
        "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        2,
    );
    let start = body.line(2);
    body.suspend_call(
        |body| {
            body.var(ILOAD, 0).var(ALOAD, 1);
        },
        false,
        "BKt",
        "one",
        ONE,
    )
    .op(ARETURN);
    let end = body.label();
    body.local("v", "I", start, end, 0);
    body.local(
        "$completion",
        "Lkotlin/coroutines/Continuation;",
        start,
        end,
        1,
    );
    let Transformed::TailCalls(method) =
        transform_named_function(body.method, &B).expect("the body transforms")
    else {
        panic!("a function whose only suspension point is a tail call needs no state machine");
    };
    assert_eq!(listing(&method), include_str!("testdata/tail_call.txt"));
}

/// ```kotlin
/// suspend fun stack(v: Int): Int = v + one(v)
/// ```
///
/// `v` is on the operand stack across the call: FixStack stores it in a local, which is spilled.
#[test]
fn a_value_on_the_stack_across_a_suspension_point_is_spilled() {
    let mut body = Body::new(
        0x0019,
        "stack",
        "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        2,
    );
    let start = body.line(3);
    body.var(ILOAD, 0)
        .suspend_call(
            |body| {
                body.var(ILOAD, 0).var(ALOAD, 1);
            },
            false,
            "BKt",
            "one",
            ONE,
        )
        .insn(Insn::Type {
            op: CHECKCAST,
            class: "java/lang/Number".to_string(),
        })
        .call(INVOKEVIRTUAL, "java/lang/Number", "intValue", "()I")
        .op(IADD)
        .call(
            INVOKESTATIC,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        .op(ARETURN);
    let end = body.label();
    body.local("v", "I", start, end, 0);
    body.local(
        "$completion",
        "Lkotlin/coroutines/Continuation;",
        start,
        end,
        1,
    );
    let Transformed::StateMachine(machine) =
        transform_named_function(body.method, &B).expect("the body transforms")
    else {
        panic!("a non-tail suspension point needs a state machine");
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = *machine;
    assert_eq!(
        listing(&method),
        include_str!("testdata/stack_at_suspension_point.txt")
    );
    let field = |name: &str| SpillField {
        name: name.to_string(),
        descriptor: "I".to_string(),
    };
    assert_eq!(layout.fields, vec![field("I$0"), field("I$1")]);
    assert_eq!(debug_metadata.line_numbers, vec![3]);
    assert_eq!(debug_metadata.next_line_numbers, vec![-1]);
    assert_eq!(debug_metadata.index_to_label, vec![0]);
    assert_eq!(debug_metadata.spilled, vec!["I$0"]);
    assert_eq!(debug_metadata.local_names, vec!["v"]);
}

/// ```kotlin
/// class P(val a: Int, val b: Int)
/// suspend fun ctor(v: Int): P = P(v, one(v))
/// ```
///
/// The uninitialized `P` cannot survive the suspension: its `new` moves past the call, the
/// arguments stored before it into locals. The expected listing is what kotlinc's
/// `CoroutineTransformerMethodVisitor` hands on, not the class file: FixStack's stores of the two
/// uninitialized copies leave slots 2 and 3 unused, and the optimizer's later
/// `removeUnusedLocalVariables` renumbers every slot above them.
#[test]
fn a_suspension_point_in_constructor_arguments_moves_the_allocation_after_it() {
    let function = NamedFunction {
        owner: "CKt",
        continuation_class: "CKt$ctor$1",
        source_file: "C.kt",
        line_number: 3,
        completion_slot: 1,
        dispatch_receiver: None,
    };
    let mut body = Body::new(
        0x0019,
        "ctor",
        "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        2,
    );
    let start = body.line(3);
    body.insn(Insn::Type {
        op: NEW,
        class: "P".to_string(),
    })
    .op(DUP)
    .var(ILOAD, 0)
    .suspend_call(
        |body| {
            body.var(ILOAD, 0).var(ALOAD, 1);
        },
        false,
        "CKt",
        "one",
        ONE,
    )
    .insn(Insn::Type {
        op: CHECKCAST,
        class: "java/lang/Number".to_string(),
    })
    .call(INVOKEVIRTUAL, "java/lang/Number", "intValue", "()I")
    .call(INVOKESPECIAL, "P", "<init>", "(II)V")
    .op(ARETURN);
    let end = body.label();
    body.local("v", "I", start, end, 0);
    body.local(
        "$completion",
        "Lkotlin/coroutines/Continuation;",
        start,
        end,
        1,
    );
    let Transformed::StateMachine(machine) =
        transform_named_function(body.method, &function).expect("the body transforms")
    else {
        panic!("a non-tail suspension point needs a state machine");
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = *machine;
    assert_eq!(
        listing(&method),
        include_str!("testdata/suspension_point_in_constructor_arguments.txt")
    );
    let field = |name: &str| SpillField {
        name: name.to_string(),
        descriptor: "I".to_string(),
    };
    assert_eq!(layout.fields, vec![field("I$0"), field("I$1")]);
    assert_eq!(debug_metadata.spilled, vec!["I$0"]);
    assert_eq!(debug_metadata.local_names, vec!["v"]);
    assert_eq!(debug_metadata.next_line_numbers, vec![-1]);
}

#[test]
fn inline_class_unboxing_is_moved_to_the_resume_path_without_removing_a_node_twice() {
    let mut body = Body::new(
        0x0019,
        "unbox",
        "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        2,
    );
    let start = body.line(3);
    body.nodes(vec![inline_call_marker(true)])
        .var(ILOAD, 0)
        .var(ALOAD, 1)
        .nodes(mark(SuspendMarker::BeforeSuspend))
        .call(INVOKESTATIC, "BKt", "one", ONE)
        .nodes(mark(SuspendMarker::AfterSuspend))
        .nodes(mark(SuspendMarker::BeforeUnboxInlineClass))
        .insn(Insn::Type {
            op: CHECKCAST,
            class: "sample/Value".to_string(),
        })
        .call(INVOKEVIRTUAL, "sample/Value", "unbox-impl", "()I")
        // This artificial cast is the end-exclusive sentinel used by kotlinc: the resume path
        // copies the real unboxing above, while marker cleanup removes this cast.
        .insn(Insn::Type {
            op: CHECKCAST,
            class: "java/lang/Object".to_string(),
        })
        .nodes(mark(SuspendMarker::AfterUnboxInlineClass))
        .op(ICONST_1)
        .op(IADD)
        .call(
            INVOKESTATIC,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        .op(ARETURN);
    let end = body.label();
    body.local("v", "I", start, end, 0);
    body.local(
        "$completion",
        "Lkotlin/coroutines/Continuation;",
        start,
        end,
        1,
    );

    let Transformed::StateMachine(machine) =
        transform_named_function(body.method, &B).expect("the body transforms")
    else {
        panic!("unboxing after a non-tail suspend call needs a state machine");
    };
    let unboxes = machine
        .method
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node,
                Node::Insn(Insn::Method { owner, name, .. })
                    if owner == "sample/Value" && name == "unbox-impl"
            )
        })
        .count();
    assert_eq!(unboxes, 1);
}

#[test]
fn a_suspension_point_rejects_an_unnormalized_operand_prefix() {
    let mut body = Body::new(
        0x0019,
        "badStack",
        "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        2,
    );
    body.op(ICONST_1)
        .var(ILOAD, 0)
        .var(ALOAD, 1)
        .nodes(mark(SuspendMarker::BeforeSuspend))
        .call(INVOKESTATIC, "BKt", "one", ONE)
        .nodes(mark(SuspendMarker::AfterSuspend))
        .insn(Insn::Type {
            op: CHECKCAST,
            class: "java/lang/Number".to_string(),
        })
        .call(INVOKEVIRTUAL, "java/lang/Number", "intValue", "()I")
        .op(IADD)
        .call(
            INVOKESTATIC,
            "java/lang/Integer",
            "valueOf",
            "(I)Ljava/lang/Integer;",
        )
        .op(ARETURN);

    assert_eq!(
        transform_named_function(body.method, &B),
        Err(CoroutineError::InvalidSuspensionResultStack { size: 2 })
    );
}

#[test]
fn completion_identity_must_name_a_physical_local() {
    let function = NamedFunction {
        completion_slot: 1,
        ..B
    };
    let method = Body::new(
        0x0001,
        "badCompletion",
        "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        1,
    )
    .method;

    assert_eq!(
        transform_named_function(method, &function),
        Err(CoroutineError::InvalidCompletionSlot {
            slot: 1,
            max_locals: 1,
        })
    );
}

#[test]
fn completion_identity_must_match_the_physical_parameter_layout() {
    let function = NamedFunction {
        completion_slot: 0,
        ..B
    };
    let method = Body::new(
        0x0019,
        "wrongCompletion",
        "(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        2,
    )
    .method;

    assert_eq!(
        transform_named_function(method, &function),
        Err(CoroutineError::CompletionSlotMismatch {
            recorded: 0,
            physical: 1,
        })
    );
}

const OBJECT: &str = "Ljava/lang/Object;";
const CONTINUATION: &str = "kotlin/coroutines/Continuation";
const INVOKE_SUSPEND: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
const LEAF: &str = "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;";

/// The start of a lambda's `invokeSuspend` as kotlinc's codegen hands it on: each parameter is
/// read from the field it was stored in, and a `mark(10)` follows its store. Returns the label
/// after the last marker, where the parameters' entries start.
fn unspill_lambda_parameters(body: &mut Body, class: &str, parameters: &[(&str, u16)]) -> LabelId {
    for &(field, slot) in parameters {
        body.var(ALOAD, 0)
            .insn(Insn::Field {
                op: GETFIELD,
                owner: class.to_string(),
                name: field.to_string(),
                desc: "I".to_string(),
            })
            .var(ISTORE, slot)
            .nodes(mark(SuspendMarker::SuspendLambdaParameter));
    }
    body.label()
}

/// `leaf()` called with the lambda itself as the continuation, its `Unit` result dropped.
fn call_leaf(body: &mut Body) {
    body.suspend_call(
        |body| {
            body.var(ALOAD, 0).insn(Insn::Type {
                op: CHECKCAST,
                class: CONTINUATION.to_string(),
            });
        },
        true,
        "AKt",
        "leaf",
        LEAF,
    )
    .op(POP);
}

fn box_int_and_return(body: &mut Body) {
    body.call(
        INVOKESTATIC,
        "java/lang/Integer",
        "valueOf",
        "(I)Ljava/lang/Integer;",
    )
    .op(ARETURN);
}

fn lambda_receivers(body: &mut Body, class: &str, start: LabelId, end: LabelId) {
    body.local("this", &format!("L{class};"), start, end, 0);
    body.local("$result", OBJECT, start, end, 1);
}

fn spill_field(name: &str, descriptor: &str) -> SpillField {
    SpillField {
        name: name.to_string(),
        descriptor: descriptor.to_string(),
    }
}

/// ```kotlin
/// suspend fun leaf() {}
/// fun f(): suspend (Int) -> Int = { x ->
///     leaf()
///     x + 1
/// }
/// ```
///
/// `x` lives in the lambda's `I$0`: it is read before the `tableswitch` on every entry, so it is
/// spilled at the call but never restored, and its entry spans the whole state machine.
#[test]
fn a_lambda_parameter_is_spilled_into_its_own_field_and_not_restored() {
    const CLASS: &str = "AKt$f$1";
    let mut body = Body::new(0x0011, "invokeSuspend", INVOKE_SUSPEND, 3);
    let start = body.label();
    let x_start = unspill_lambda_parameters(&mut body, CLASS, &[("I$0", 2)]);
    body.line(4);
    call_leaf(&mut body);
    body.line(5);
    body.var(ILOAD, 2).op(ICONST_1).op(IADD);
    box_int_and_return(&mut body);
    let end = body.label();
    body.local("x", "I", x_start, end, 2);
    lambda_receivers(&mut body, CLASS, start, end);

    let lambda = SuspendLambda {
        class: CLASS,
        source_file: "A.kt",
        line_number: 3,
        declared_spill_fields: &[DeclaredSpillFields {
            descriptor: "I",
            max_index: 0,
        }],
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = transform_suspend_lambda(body.method, &lambda).expect("the body transforms");
    // kotlinc 2.4.20's `AKt$f$1.invokeSuspend`.
    assert_eq!(
        listing(&method),
        include_str!("testdata/lambda_parameter.txt")
    );
    assert_eq!(method.max_locals, 4);
    assert_eq!(layout.fields, Vec::new());
    assert_eq!(debug_metadata.source_file, "A.kt");
    assert_eq!(debug_metadata.line_numbers, vec![4]);
    assert_eq!(debug_metadata.next_line_numbers, vec![5]);
    assert_eq!(debug_metadata.index_to_label, vec![0]);
    assert_eq!(debug_metadata.spilled, vec!["I$0"]);
    assert_eq!(debug_metadata.local_names, vec!["x"]);
    assert_eq!(debug_metadata.method_name, "invokeSuspend");
    assert_eq!(debug_metadata.class_name, CLASS);
}

/// ```kotlin
/// fun g(): suspend () -> Int = {
///     val a = "s".length
///     leaf()
///     a
/// }
/// ```
///
/// Without parameters the machine starts at the body's first instruction, and every spill field
/// is the machine's own.
#[test]
fn a_lambda_without_parameters_spills_into_new_fields() {
    const CLASS: &str = "AKt$g$1";
    let mut body = Body::new(0x0011, "invokeSuspend", INVOKE_SUSPEND, 3);
    let start = body.label();
    body.line(9);
    body.op(ICONST_1).var(ISTORE, 2);
    let a_start = body.line(10);
    call_leaf(&mut body);
    body.line(11);
    body.var(ILOAD, 2);
    box_int_and_return(&mut body);
    let end = body.label();
    body.local("a", "I", a_start, end, 2);
    lambda_receivers(&mut body, CLASS, start, end);

    let lambda = SuspendLambda {
        class: CLASS,
        source_file: "A.kt",
        line_number: 8,
        declared_spill_fields: &[],
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = transform_suspend_lambda(body.method, &lambda).expect("the body transforms");
    // kotlinc 2.4.20's `AKt$g$1.invokeSuspend`.
    assert_eq!(
        listing(&method),
        include_str!("testdata/lambda_without_parameters.txt")
    );
    assert_eq!(layout.fields, vec![spill_field("I$0", "I")]);
    assert_eq!(debug_metadata.line_numbers, vec![10]);
    assert_eq!(debug_metadata.next_line_numbers, vec![11]);
    assert_eq!(debug_metadata.spilled, vec!["I$0"]);
    assert_eq!(debug_metadata.local_names, vec!["a"]);
}

/// ```kotlin
/// fun h(): suspend (Int) -> Int = { x ->
///     val s = "s" + x
///     val a = s.length
///     leaf()
///     x + a + s.length
/// }
/// ```
///
/// The class already declares `I$0` for `x` (`initialVarsCountByType`): the machine adds only the
/// fields past it, after the kinds the class declares.
#[test]
fn a_lambda_declares_only_the_spill_fields_past_its_parameters() {
    const CLASS: &str = "AKt$h$1";
    let mut body = Body::new(0x0011, "invokeSuspend", INVOKE_SUSPEND, 5);
    let start = body.label();
    let x_start = unspill_lambda_parameters(&mut body, CLASS, &[("I$0", 2)]);
    body.line(15);
    body.insn(Insn::Type {
        op: NEW,
        class: STRING_BUILDER.to_string(),
    })
    .op(DUP)
    .call(INVOKESPECIAL, STRING_BUILDER, "<init>", "()V")
    .insn(Insn::Int {
        op: BIPUSH,
        operand: 115,
    })
    .call(
        INVOKEVIRTUAL,
        STRING_BUILDER,
        "append",
        "(C)Ljava/lang/StringBuilder;",
    )
    .var(ILOAD, 2)
    .call(
        INVOKEVIRTUAL,
        STRING_BUILDER,
        "append",
        "(I)Ljava/lang/StringBuilder;",
    )
    .call(
        INVOKEVIRTUAL,
        STRING_BUILDER,
        "toString",
        "()Ljava/lang/String;",
    )
    .var(ASTORE, 3);
    let s_start = body.line(16);
    body.var(ALOAD, 3)
        .call(INVOKEVIRTUAL, STRING, "length", "()I")
        .var(ISTORE, 4);
    let a_start = body.line(17);
    call_leaf(&mut body);
    body.line(18);
    body.var(ILOAD, 2)
        .var(ILOAD, 4)
        .op(IADD)
        .var(ALOAD, 3)
        .call(INVOKEVIRTUAL, STRING, "length", "()I")
        .op(IADD);
    box_int_and_return(&mut body);
    let end = body.label();
    body.local("x", "I", x_start, end, 2);
    body.local("s", "Ljava/lang/String;", s_start, end, 3);
    body.local("a", "I", a_start, end, 4);
    lambda_receivers(&mut body, CLASS, start, end);

    let lambda = SuspendLambda {
        class: CLASS,
        source_file: "A.kt",
        line_number: 14,
        declared_spill_fields: &[DeclaredSpillFields {
            descriptor: "I",
            max_index: 0,
        }],
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = transform_suspend_lambda(body.method, &lambda).expect("the body transforms");
    // kotlinc 2.4.20's `AKt$h$1.invokeSuspend`.
    assert_eq!(
        listing(&method),
        include_str!("testdata/lambda_parameter_field_reused.txt")
    );
    assert_eq!(
        layout.fields,
        vec![spill_field("I$1", "I"), spill_field("L$0", OBJECT)]
    );
    assert_eq!(debug_metadata.line_numbers, vec![17]);
    assert_eq!(debug_metadata.next_line_numbers, vec![18]);
    assert_eq!(debug_metadata.index_to_label, vec![0, 0, 0]);
    assert_eq!(debug_metadata.spilled, vec!["L$0", "I$0", "I$1"]);
    assert_eq!(debug_metadata.local_names, vec!["s", "x", "a"]);
}

/// ```kotlin
/// suspend fun leaf() {}
/// fun use(x: String) {}
/// fun f(): suspend (String) -> Unit = { s ->
///     use(s)
///     leaf()
/// }
/// ```
///
/// `s` lives in the lambda's `L$0` and is dead at the call, so it is spilled back through
/// `nullOutSpilledVariable`. Every declared parameter field is spilled at every point, so the
/// point never nulls a field the class declares.
#[test]
fn a_dead_reference_parameter_is_spilled_back_into_its_field() {
    const CLASS: &str = "AKt$f$1";
    let mut body = Body::new(0x0011, "invokeSuspend", INVOKE_SUSPEND, 3);
    let start = body.label();
    body.var(ALOAD, 0)
        .insn(Insn::Field {
            op: GETFIELD,
            owner: CLASS.to_string(),
            name: "L$0".to_string(),
            desc: OBJECT.to_string(),
        })
        .insn(Insn::Type {
            op: CHECKCAST,
            class: STRING.to_string(),
        })
        .var(ASTORE, 2)
        .nodes(mark(SuspendMarker::SuspendLambdaParameter));
    let s_start = body.label();
    body.line(4);
    body.var(ALOAD, 2)
        .call(INVOKESTATIC, "AKt", "use", "(Ljava/lang/String;)V");
    body.line(5);
    call_leaf(&mut body);
    body.line(6);
    body.insn(Insn::Field {
        op: GETSTATIC,
        owner: "kotlin/Unit".to_string(),
        name: "INSTANCE".to_string(),
        desc: "Lkotlin/Unit;".to_string(),
    })
    .op(ARETURN);
    let end = body.label();
    body.local("s", "Ljava/lang/String;", s_start, end, 2);
    lambda_receivers(&mut body, CLASS, start, end);

    let lambda = SuspendLambda {
        class: CLASS,
        source_file: "A.kt",
        line_number: 3,
        declared_spill_fields: &[DeclaredSpillFields {
            descriptor: OBJECT,
            max_index: 0,
        }],
    };
    let StateMachine {
        method,
        layout,
        debug_metadata,
    } = transform_suspend_lambda(body.method, &lambda).expect("the body transforms");
    // kotlinc 2.4.20's `AKt$f$1.invokeSuspend`.
    assert_eq!(
        listing(&method),
        include_str!("testdata/lambda_dead_reference_parameter.txt")
    );
    assert_eq!(layout.fields, Vec::new());
    assert_eq!(debug_metadata.spilled, vec!["L$0"]);
    assert_eq!(debug_metadata.local_names, vec!["s"]);
}

#[test]
fn a_lambda_body_must_be_an_instance_invoke_suspend() {
    let lambda = SuspendLambda {
        class: "AKt$f$1",
        source_file: "A.kt",
        line_number: 3,
        declared_spill_fields: &[],
    };
    let method = Body::new(0x0019, "invokeSuspend", INVOKE_SUSPEND, 1).method;
    assert_eq!(
        transform_suspend_lambda(method, &lambda),
        Err(CoroutineError::Unsupported(
            "a suspend lambda body that is not an instance `invokeSuspend(Object): Object`"
        ))
    );
}
