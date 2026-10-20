//! An omitted inline lambda parameter's default, inlined from the lambda class a dependency
//! compiled for it: kotlinc's `DefaultLambda`.
//!
//! The `$default` bridge's arm loads the class's singleton ([`super::ExtractedDefaultLambda`]); the
//! call inlines the class's erased `invoke(Object…)Object` wherever the body invokes the
//! parameter (`loadDefaultLambdaBody`). When that `invoke` is the bridge to a typed one, the typed
//! body is copied into it (`inlineBridge`). The lambda object itself is the skipped `this` of the
//! body, which takes no slot of the caller, so the body is written without it: its parameters
//! start at slot 0, like those of a literal lambda's node. The body then gets the marker local
//! `$i$a$-<function>-<class>` (`createNodeWithFakeVariables`), and its reified operations are
//! specialized when the arm asked for it.

use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::class_node::ClassNode;
use crate::jvm::method_node::{Insn, LabelId, LocalVariable, MethodNode, Node};
use crate::jvm::reified_arguments::ReifiedArguments;

use super::{ExtractedDefaultLambda, InlineError, Lambda};

const ICONST_0: u8 = 0x03;
const ISTORE: u8 = 0x36;
const ALOAD: u8 = 0x19;
const GOTO: u8 = 0xa7;
const INVOKESTATIC: u8 = 0xb8;
const ACC_STATIC: u16 = 0x0008;
const ACC_BRIDGE: u16 = 0x0040;
const OBJECT: &str = "Ljava/lang/Object;";

/// The super classes of the lambda classes a default lambda cannot be: a suspend lambda's body is
/// its state machine, and a callable reference reads its target through its own members.
const UNSUPPORTED_SUPER_CLASSES: [&str; 13] = [
    "kotlin/coroutines/jvm/internal/SuspendLambda",
    "kotlin/coroutines/jvm/internal/RestrictedSuspendLambda",
    "kotlin/jvm/internal/FunctionReference",
    "kotlin/jvm/internal/FunctionReferenceImpl",
    "kotlin/jvm/internal/AdaptedFunctionReference",
    "kotlin/jvm/internal/PropertyReference0",
    "kotlin/jvm/internal/PropertyReference1",
    "kotlin/jvm/internal/PropertyReference2",
    "kotlin/jvm/internal/MutablePropertyReference0",
    "kotlin/jvm/internal/MutablePropertyReference1",
    "kotlin/jvm/internal/MutablePropertyReference2",
    "kotlin/jvm/internal/PropertyReference0Impl",
    "kotlin/jvm/internal/PropertyReference1Impl",
];

/// The lambda `extracted` names, read from its class: the body every `invoke` of the parameter
/// is replaced with. `function` is the inline function's name, without `$default`.
pub(crate) fn load(
    class: &ClassNode,
    extracted: &ExtractedDefaultLambda,
    function: &str,
    reified: &ReifiedArguments,
) -> Result<Lambda, InlineError> {
    if class
        .super_name
        .as_deref()
        .is_some_and(|super_name| UNSUPPORTED_SUPER_CLASSES.contains(&super_name))
    {
        return Err(unsupported(
            "a default lambda that is not a plain lambda class",
        ));
    }
    let erased = class
        .methods
        .iter()
        .find(|method| {
            method.name == "invoke"
                && descriptors::return_type(&method.desc) == Some(OBJECT)
                && descriptors::argument_types(&method.desc)
                    .is_some_and(|arguments| arguments.iter().all(|argument| *argument == OBJECT))
        })
        .ok_or_else(|| malformed("a default lambda class has no erased invoke"))?;
    if erased.access & ACC_STATIC != 0 {
        return Err(malformed("a default lambda's invoke is static"));
    }
    let mut node = erased
        .code
        .clone()
        .ok_or_else(|| malformed("a default lambda's invoke has no code"))?;
    if erased.access & ACC_BRIDGE != 0 {
        inline_bridge(&mut node, class)?;
    }
    drop_receiver(&mut node)?;
    add_marker_variable(&mut node, function, &class.name);
    if extracted.need_reification {
        super::reified::specialize(
            &mut node,
            reified,
            &mut crate::jvm::reified_arguments::NoEnumEntriesReads,
        )?;
    }
    if node.instructions().any(is_class_reification_marker) {
        return Err(unsupported(
            "a default lambda whose objects need reification",
        ));
    }
    let parameter_types: Vec<String> = descriptors::argument_types(&node.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?
        .into_iter()
        .map(str::to_string)
        .collect();
    let return_type = descriptors::return_type(&node.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?
        .to_string();
    node.access |= ACC_STATIC;
    Ok(Lambda {
        value_class_parameters: vec![None; parameter_types.len()],
        parameter_types,
        return_type,
        value_class_return: None,
        captured: 0..0,
        capture_names: Vec::new(),
        default_class: Some(class.name.clone()),
        node,
    })
}

/// `inlineBridge`: the erased `invoke` only adapts its arguments for the typed one it calls on
/// `this`, so the typed body replaces that call, its parameters stored from the adapted values and
/// its returns jumping to what the bridge does with the result.
fn inline_bridge(bridge: &mut MethodNode, class: &ClassNode) -> Result<(), InlineError> {
    let mut calls = bridge.nodes.iter().enumerate().filter(|(_, entry)| {
        matches!(entry, Node::Insn(Insn::Method { owner, .. }) if *owner == class.name)
    });
    let (call, entry) = calls
        .next()
        .ok_or_else(|| malformed("a default lambda's bridge calls no invoke of its class"))?;
    if calls.next().is_some() {
        return Err(malformed(
            "a default lambda's bridge calls its class more than once",
        ));
    }
    let Node::Insn(Insn::Method { name, desc, .. }) = entry else {
        unreachable!("the call is a method instruction")
    };
    let target = class
        .methods
        .iter()
        .find(|method| method.name == *name && method.desc == *desc)
        .ok_or_else(|| malformed("a default lambda's bridge calls an invoke its class lacks"))?;
    if target.access & ACC_STATIC != 0 {
        return Err(unsupported("a default lambda whose typed invoke is static"));
    }
    let typed = target
        .code
        .as_ref()
        .ok_or_else(|| malformed("a default lambda's typed invoke has no code"))?;
    let arguments = descriptors::argument_types(&target.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?;
    let arguments_size = arguments
        .iter()
        .map(|argument| descriptors::size(argument) as u16)
        .sum::<u16>()
        + 1;

    // The typed body's labels join the bridge's.
    let mut labels = Vec::with_capacity(typed.label_count());
    for _ in 0..typed.label_count() {
        labels.push(bridge.new_label());
    }
    let label = |label: LabelId| labels[label.index()];
    let invoke_label = bridge.new_label();
    let return_label = bridge.new_label();

    let mut inserted = Vec::new();
    let mut offset = arguments_size;
    for argument in arguments.iter().rev() {
        offset -= descriptors::size(argument) as u16;
        inserted.push(Node::Insn(Insn::Var {
            op: descriptors::typed_opcode(argument, ISTORE),
            slot: offset,
        }));
    }
    // The bridge loads `this` as the receiver of the call it makes. kotlinc pops it here, then
    // deletes both as moves of the skipped lambda parameter; neither is written.
    inserted.push(Node::Label(invoke_label));
    for entry in &typed.nodes {
        inserted.push(match entry {
            Node::Label(original) => Node::Label(label(*original)),
            Node::Line { line, start } => Node::Line {
                line: *line,
                start: label(*start),
            },
            Node::Insn(Insn::Op(0xac..=0xb1)) => Node::Insn(Insn::Jump {
                op: GOTO,
                target: return_label,
            }),
            Node::Insn(insn) => Node::Insn(relabel(insn, &label)),
        });
    }
    inserted.push(Node::Label(return_label));
    let receiver = bridge
        .nodes
        .iter()
        .position(|entry| matches!(entry, Node::Insn(Insn::Var { op: ALOAD, slot: 0 })))
        .filter(|&at| at < call)
        .ok_or_else(|| malformed("a default lambda's bridge does not load its receiver"))?;
    let mut nodes = Vec::with_capacity(bridge.nodes.len() + inserted.len());
    for (at, entry) in std::mem::take(&mut bridge.nodes).into_iter().enumerate() {
        if at == call {
            nodes.append(&mut inserted);
        } else if at != receiver && !matches!(entry, Node::Line { .. }) {
            nodes.push(entry);
        }
    }
    bridge.nodes = nodes;
    bridge.local_variables = typed
        .local_variables
        .iter()
        .map(|local| {
            let (start, end) = if local.slot < arguments_size {
                (invoke_label, return_label)
            } else {
                (label(local.start), label(local.end))
            };
            LocalVariable {
                start,
                end,
                ..local.clone()
            }
        })
        .collect();
    bridge.try_catch_blocks = typed
        .try_catch_blocks
        .iter()
        .map(|block| crate::jvm::method_node::TryCatchBlock {
            start: label(block.start),
            end: label(block.end),
            handler: label(block.handler),
            catch_type: block.catch_type.clone(),
        })
        .collect();
    bridge.max_locals = bridge.max_locals.max(typed.max_locals);
    bridge.max_stack = bridge.max_stack.max(typed.max_stack);
    Ok(())
}

/// `insn` with its labels renamed by `label`.
fn relabel(insn: &Insn, label: &dyn Fn(LabelId) -> LabelId) -> Insn {
    match insn {
        Insn::Jump { op, target } => Insn::Jump {
            op: *op,
            target: label(*target),
        },
        Insn::TableSwitch {
            low,
            high,
            default,
            labels,
        } => Insn::TableSwitch {
            low: *low,
            high: *high,
            default: label(*default),
            labels: labels.iter().map(|target| label(*target)).collect(),
        },
        Insn::LookupSwitch {
            default,
            keys,
            labels,
        } => Insn::LookupSwitch {
            default: label(*default),
            keys: keys.clone(),
            labels: labels.iter().map(|target| label(*target)).collect(),
        },
        other => other.clone(),
    }
}

/// The skipped `this` takes no slot: every other local moves down one word. A body that still
/// reads the lambda object is not inlined yet.
fn drop_receiver(node: &mut MethodNode) -> Result<(), InlineError> {
    for entry in &mut node.nodes {
        if let Node::Insn(Insn::Var { slot, .. } | Insn::Iinc { slot, .. }) = entry {
            *slot = slot
                .checked_sub(1)
                .ok_or_else(|| unsupported("a default lambda that reads its own object"))?;
        }
    }
    node.local_variables.retain(|local| local.slot != 0);
    for local in &mut node.local_variables {
        local.slot -= 1;
    }
    node.max_locals = node.max_locals.saturating_sub(1);
    Ok(())
}

/// `createNodeWithFakeVariables`: the marker local kotlinc names an inlined lambda by, set before
/// the body and live to its end.
fn add_marker_variable(node: &mut MethodNode, function: &str, class: &str) {
    let slot = node.max_locals;
    let start = node.new_label();
    node.nodes.splice(
        0..0,
        [
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var { op: ISTORE, slot }),
            Node::Label(start),
        ],
    );
    let end = match node.nodes.last() {
        Some(Node::Label(end)) => *end,
        _ => {
            let end = node.new_label();
            node.nodes.push(Node::Label(end));
            end
        }
    };
    let simple = class.rsplit('/').next().unwrap_or(class);
    node.local_variables.push(LocalVariable {
        name: format!("$i$a$-{function}-{simple}"),
        desc: "I".to_string(),
        start,
        end,
        slot,
    });
    node.max_locals += 1;
}

fn is_class_reification_marker(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Method { op: INVOKESTATIC, owner, name, .. }
            if owner == "kotlin/jvm/internal/Intrinsics" && name == "needClassReification"
    )
}

fn unsupported(reason: &'static str) -> InlineError {
    InlineError::DefaultLambda(reason)
}

fn malformed(detail: &str) -> InlineError {
    InlineError::Analysis(detail.to_owned())
}
