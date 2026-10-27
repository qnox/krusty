use super::*;
use crate::jvm::bytecode_passes::opcodes::*;
use crate::jvm::method_node::{Constant, Insn, LabelId, MethodNode, Node, TryCatchBlock};

mod stdlib;

const ACC_STATIC: u16 = 0x0008;

/// A static method `f``desc` with `max_locals` slots and `labels` labels, its nodes built against
/// those labels.
fn method(
    desc: &str,
    max_locals: u16,
    labels: usize,
    build: impl FnOnce(&[LabelId]) -> Vec<Node>,
) -> MethodNode {
    let mut node = MethodNode::new(ACC_STATIC, "f", desc);
    let labels: Vec<LabelId> = (0..labels).map(|_| node.new_label()).collect();
    node.nodes = build(&labels);
    node.max_locals = max_locals;
    node.max_stack = 8;
    node
}

fn op(op: u8) -> Node {
    Node::Insn(Insn::Op(op))
}

fn var(op: u8, slot: u16) -> Node {
    Node::Insn(Insn::Var { op, slot })
}

fn jump(op: u8, target: LabelId) -> Node {
    Node::Insn(Insn::Jump { op, target })
}

/// The decoded body as text: one line per local, block, operation and terminator, so a test
/// asserts the whole body at once.
pub(super) fn render(body: &DecodedBody) -> String {
    let mut out = String::new();
    let values = |ids: &[ValueId]| {
        ids.iter()
            .map(|id| format!("v{}", id.0))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let edge = |edge: &Edge| format!("b{}({})", edge.target.0, values(&edge.arguments));
    let ty = |id: ValueId| format!("{:?}", body.value(id).ty);
    for (index, local) in body.locals.iter().enumerate() {
        let parameter = if body.parameters.contains(&LocalId(index as u32)) {
            " parameter"
        } else {
            ""
        };
        out += &format!(
            "l{index} = slot {} {:?}{parameter}\n",
            local.slot, local.category
        );
    }
    for (index, block) in body.blocks.iter().enumerate() {
        let parameters: Vec<String> = block
            .parameters
            .iter()
            .map(|&id| format!("v{}: {}", id.0, ty(id)))
            .collect();
        out += &format!("b{index}({}):\n", parameters.join(", "));
        for op in &block.ops {
            let line = op.line.map_or("-".to_string(), |line| line.to_string());
            let result = op
                .result
                .map_or(String::new(), |id| format!("v{}: {} = ", id.0, ty(id)));
            let kind = match &op.kind {
                OpKind::LoadLocal(local) => format!("load l{}", local.0),
                OpKind::StoreLocal { local, value } => format!("store l{} v{}", local.0, value.0),
                OpKind::Invoke {
                    kind,
                    target,
                    arguments,
                } => format!(
                    "invoke {kind:?} {}.{}{}{} ({})",
                    target.owner,
                    target.name,
                    target.descriptor,
                    if target.interface { " interface" } else { "" },
                    values(arguments)
                ),
                other => format!("{other:?}"),
            };
            out += &format!("  [{line}] {result}{kind}\n");
        }
        let line = block
            .terminator_line
            .map_or("-".to_string(), |line| line.to_string());
        let terminator = match &block.terminator {
            Terminator::Goto(target) => format!("goto {}", edge(target)),
            Terminator::Branch {
                condition,
                taken,
                not_taken,
            } => format!(
                "branch {condition:?} ? {} : {}",
                edge(taken),
                edge(not_taken)
            ),
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let cases: Vec<String> = cases
                    .iter()
                    .map(|(key, target)| format!("{key} => {}", edge(target)))
                    .collect();
                format!(
                    "switch v{} [{}] default {}",
                    value.0,
                    cases.join(", "),
                    edge(default)
                )
            }
            Terminator::Return(None) => "return".to_string(),
            Terminator::Return(Some(value)) => format!("return v{}", value.0),
            Terminator::Throw(value) => format!("throw v{}", value.0),
        };
        out += &format!("  [{line}] {terminator}\n");
    }
    for handler in &body.handlers {
        let covered: Vec<String> = handler
            .covered
            .iter()
            .map(|block| format!("b{}", block.0))
            .collect();
        out += &format!(
            "handler [{}] -> b{} catch {:?}\n",
            covered.join(", "),
            handler.handler.0,
            handler.catch_type
        );
    }
    for scope in &body.local_scopes {
        out += &format!(
            "scope l{} {} {} {:?}..{:?}\n",
            scope.local.0, scope.name, scope.descriptor, scope.start, scope.end
        );
    }
    out
}

fn decoded(method: &MethodNode) -> String {
    render(&decode("t/T", method).expect("decode"))
}

/// A value left on the stack by both arms of a conditional reaches the join as a block parameter,
/// typed by the merged frame.
#[test]
fn stack_value_across_a_join_is_a_block_parameter() {
    let body = method("(I)I", 1, 2, |labels| {
        let [else_, join] = [labels[0], labels[1]];
        vec![
            var(ILOAD, 0),
            jump(IFEQ, else_),
            op(ICONST_1),
            jump(GOTO, join),
            Node::Label(else_),
            op(ICONST_0 + 2),
            Node::Label(join),
            op(IRETURN),
        ]
    });
    assert_eq!(
        decoded(&body),
        "\
l0 = slot 0 Int parameter
b0():
  [-] v0: Int = load l0
  [-] branch IntZero { comparison: Eq, value: ValueId(0) } ? b2() : b1()
b1():
  [-] v1: Int = Constant(Int(1))
  [-] goto b3(v1)
b2():
  [-] v2: Int = Constant(Int(2))
  [-] goto b3(v2)
b3(v3: Int):
  [-] return v3
"
    );
}

/// `dup2` copies one two-word value; `dup_x2`, `pop` and `pop2` move values by their words.
#[test]
fn stack_manipulations_move_values_by_words() {
    let doubled = method("(J)J", 2, 0, |_| {
        vec![var(LLOAD, 0), op(DUP2), op(0x61), op(0xad)]
    });
    assert_eq!(
        decoded(&doubled),
        "\
l0 = slot 0 Long parameter
b0():
  [-] v0: Long = load l0
  [-] v1: Long = Binary { op: Add, category: Long, lhs: ValueId(0), rhs: ValueId(0) }
  [-] return v1
"
    );
    let shuffled = method("(IJ)I", 3, 0, |_| {
        vec![
            var(LLOAD, 1),
            var(ILOAD, 0),
            op(DUP_X2),
            op(POP),
            op(POP2),
            op(IRETURN),
        ]
    });
    assert_eq!(
        decoded(&shuffled),
        "\
l0 = slot 0 Int parameter
l1 = slot 1 Long parameter
b0():
  [-] v0: Long = load l1
  [-] v1: Int = load l0
  [-] return v1
"
    );
}

#[test]
fn a_stack_manipulation_splitting_a_wide_value_is_rejected() {
    let body = method("(J)V", 2, 0, |_| vec![var(LLOAD, 0), op(POP), op(RETURN)]);
    assert_eq!(
        decode("t/T", &body),
        Err(DecodeError::Analysis {
            node: 1,
            message: "a one-word value was expected".to_string(),
        })
    );
}

/// A protected range covers the blocks inside it; the handler block receives the caught exception
/// as its one parameter, typed by the catch type.
#[test]
fn try_catch_ranges_become_handlers_over_blocks() {
    let mut body = method("()V", 1, 3, |labels| {
        let [start, end, handler] = [labels[0], labels[1], labels[2]];
        vec![
            Node::Label(start),
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "a/B".to_string(),
                name: "f".to_string(),
                desc: "()V".to_string(),
                interface: false,
            }),
            Node::Label(end),
            op(RETURN),
            Node::Label(handler),
            var(ASTORE, 0),
            var(ALOAD, 0),
            op(ATHROW),
        ]
    });
    let placed: Vec<LabelId> = body
        .nodes
        .iter()
        .filter_map(|node| match node {
            Node::Label(label) => Some(*label),
            _ => None,
        })
        .collect();
    let [start, end, handler] = [placed[0], placed[1], placed[2]];
    body.try_catch_blocks.push(TryCatchBlock {
        start,
        end,
        handler,
        catch_type: Some("java/lang/Exception".to_string()),
    });
    assert_eq!(
        decoded(&body),
        "\
l0 = slot 0 Reference
b0():
  [-] invoke Static a/B.f()V ()
  [-] goto b1()
b1():
  [-] return
b2(v0: Reference(\"Ljava/lang/Exception;\")):
  [-] store l0 v0
  [-] v1: Reference(\"Ljava/lang/Exception;\") = load l0
  [-] throw v1
handler [b0] -> b2 catch Some(\"java/lang/Exception\")
"
    );
}

#[test]
fn switch_cases_keep_their_keys_and_order() {
    let body = method("(I)I", 1, 3, |labels| {
        let [one, two, other] = [labels[0], labels[1], labels[2]];
        vec![
            var(ILOAD, 0),
            Node::Insn(Insn::TableSwitch {
                low: 1,
                high: 2,
                default: other,
                labels: vec![one, two],
            }),
            Node::Label(one),
            op(ICONST_1),
            op(IRETURN),
            Node::Label(two),
            Node::Insn(Insn::Ldc(Constant::Int(20))),
            op(IRETURN),
            Node::Label(other),
            op(ICONST_0),
            op(IRETURN),
        ]
    });
    assert_eq!(
        decoded(&body),
        "\
l0 = slot 0 Int parameter
b0():
  [-] v0: Int = load l0
  [-] switch v0 [1 => b1(), 2 => b2()] default b3()
b1():
  [-] v1: Int = Constant(Int(1))
  [-] return v1
b2():
  [-] v2: Int = Constant(Int(20))
  [-] return v2
b3():
  [-] v3: Int = Constant(Int(0))
  [-] return v3
"
    );
}

/// Code no path reaches is left out; a slot reused with another category is another local.
#[test]
fn unreached_code_is_dropped_and_reused_slots_split_by_category() {
    let body = method("()J", 2, 1, |labels| {
        let live = labels[0];
        vec![
            op(ICONST_0),
            var(ISTORE, 0),
            jump(GOTO, live),
            op(ICONST_1),
            op(ATHROW),
            Node::Label(live),
            op(LCONST_1),
            var(0x37, 0),
            var(LLOAD, 0),
            op(0xad),
        ]
    });
    assert_eq!(
        decoded(&body),
        "\
l0 = slot 0 Int
l1 = slot 0 Long
b0():
  [-] v0: Int = Constant(Int(0))
  [-] store l0 v0
  [-] goto b1()
b1():
  [-] v1: Long = Constant(Long(1))
  [-] store l1 v1
  [-] v2: Long = load l1
  [-] return v2
"
    );
}

/// An untyped load (a slot nothing stored to) is the analyzer's error, never a guessed type.
#[test]
fn a_load_of_an_unassigned_local_is_rejected() {
    let body = method("()I", 1, 0, |_| vec![var(ILOAD, 0), op(IRETURN)]);
    assert_eq!(
        decode("t/T", &body),
        Err(DecodeError::Malformed {
            node: 0,
            reason: "load of an unassigned local",
        })
    );
}
