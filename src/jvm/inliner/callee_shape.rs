//! Questions the call site asks of a callee body before inlining it.

use std::collections::HashSet;

use crate::jvm::method_node::{Category, Insn, MethodNode, Node};

use super::preparation::label_positions;

const ACC_STATIC: u16 = 0x0008;
const GETSTATIC: u8 = 0xb2;
const PUTSTATIC: u8 = 0xb3;
const INVOKESTATIC: u8 = 0xb8;
const NEW: u8 = 0xbb;

/// A body shape the ported stages do not inline yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnsupportedShape {
    /// It constructs or loads an anonymous object or SAM wrapper, which kotlinc regenerates as a
    /// `$$inlined$` class for the call site (`AnonymousObjectTransformer`).
    AnonymousObject,
    /// It reads a `$WhenMappings` table, which kotlinc regenerates for the call site.
    WhenMappings,
    /// It reads `$assertionsDisabled`, a field kotlinc moves to the calling class.
    AssertionsStatus,
    /// It materializes a class whose shape depends on a reified parameter. The anonymous-object
    /// regeneration stage must copy that class before the marker can be removed.
    ClassReification,
}

/// The first shape in `callee` that needs a later stage of the port, if any.
pub(crate) fn unsupported_shape(callee: &MethodNode) -> Option<UnsupportedShape> {
    callee.instructions().find_map(|insn| match insn {
        Insn::Type { op: NEW, class } if is_anonymous_class(class) || is_sam_wrapper(class) => {
            Some(UnsupportedShape::AnonymousObject)
        }
        Insn::Method { name, owner, .. }
            if name == "<init>" && (is_anonymous_class(owner) || is_sam_wrapper(owner)) =>
        {
            Some(UnsupportedShape::AnonymousObject)
        }
        Insn::Field {
            op: GETSTATIC,
            owner,
            name,
            desc,
        } => {
            if name == "INSTANCE" && is_anonymous_class(owner) {
                Some(UnsupportedShape::AnonymousObject)
            } else if name.starts_with("$EnumSwitchMapping$") && owner.ends_with("$WhenMappings") {
                Some(UnsupportedShape::WhenMappings)
            } else if name == "$assertionsDisabled" && desc == "Z" {
                Some(UnsupportedShape::AssertionsStatus)
            } else {
                None
            }
        }
        Insn::Method {
            op: INVOKESTATIC,
            owner,
            name,
            desc,
            interface,
        } if owner == "kotlin/jvm/internal/Intrinsics"
            && !interface
            && name == "needClassReification"
            && desc == "()V" =>
        {
            Some(UnsupportedShape::ClassReification)
        }
        _ => None,
    })
}

/// kotlinc's `isAnonymousClass`: a class whose simple name ends in `$<number>`, SAM wrappers aside.
fn is_anonymous_class(internal: &str) -> bool {
    if internal.contains("$sam$") {
        return false;
    }
    let simple = internal.rsplit('/').next().unwrap_or(internal);
    simple
        .rsplit_once('$')
        .is_some_and(|(_, suffix)| suffix.parse::<i32>().is_ok())
}

/// kotlinc's `isSamWrapper`, current and pre-1.2.30 templates alike.
fn is_sam_wrapper(internal: &str) -> bool {
    internal.contains("$sam$")
}

/// `requiresEmptyStackOnEntry`: a body with try/catch blocks or a backward jump (a loop) is entered
/// with nothing under it on the stack, which kotlinc arranges by spilling the caller's stack.
pub(crate) fn requires_empty_stack_on_entry(callee: &MethodNode) -> bool {
    if !callee.try_catch_blocks.is_empty() {
        return true;
    }
    let positions = label_positions(callee);
    let before = |from: usize, label: &crate::jvm::method_node::LabelId| {
        positions[label.index()].is_some_and(|to| to < from)
    };
    callee
        .nodes
        .iter()
        .enumerate()
        .any(|(at, entry)| match entry {
            Node::Insn(Insn::Jump { target, .. }) => before(at, target),
            Node::Insn(Insn::TableSwitch {
                default, labels, ..
            })
            | Node::Insn(Insn::LookupSwitch {
                default, labels, ..
            }) => std::iter::once(default)
                .chain(labels)
                .any(|label| before(at, label)),
            _ => false,
        })
}

/// kotlinc's `canInlineArgumentsInPlace` over the callee's own body: its parameters are loaded
/// first, in declaration order, each once (or as a run of identical loads), with nothing that could
/// observe the order in between, and never touched again. The call site then evaluates each
/// argument where its parameter is loaded instead of storing it to a temporary.
pub(crate) fn can_inline_arguments_in_place(callee: &MethodNode) -> bool {
    let Some(mut sizes) =
        crate::jvm::names::parse_method_descriptor(&callee.desc).map(|(parameters, _)| {
            parameters
                .iter()
                .map(|parameter| Category::of_descriptor(parameter).words() as u16)
                .collect::<Vec<_>>()
        })
    else {
        return false;
    };
    if callee.access & ACC_STATIC == 0 {
        sizes.insert(0, 1);
    }
    let argument_end: u16 = sizes.iter().sum();
    let protected_starts: HashSet<_> = callee.try_catch_blocks.iter().map(|b| b.start).collect();
    let nodes = &callee.nodes;
    let mut expected = 0u16;
    let mut argument = 0usize;
    let mut at = 0usize;
    while at < nodes.len() && expected < argument_end {
        let insn = match &nodes[at] {
            Node::Label(label) if protected_starts.contains(label) => return false,
            Node::Insn(insn) => insn,
            _ => {
                at += 1;
                continue;
            }
        };
        if prohibited_during_arguments(insn) {
            return false;
        }
        if let Insn::Field {
            op: GETSTATIC,
            owner,
            name,
            desc,
        } = insn
        {
            let whitelisted = matches!(
                (owner.as_str(), name.as_str(), desc.as_str()),
                ("kotlin/Result", "Companion", "Lkotlin/Result$Companion;")
                    | ("kotlin/_Assertions", "ENABLED", "Z")
            );
            if !whitelisted {
                return false;
            }
        }
        match insn {
            Insn::Var {
                op: 0x36..=0x3a,
                slot,
            }
            | Insn::Iinc { slot, .. }
                if *slot < argument_end =>
            {
                return false
            }
            Insn::Var {
                op: op @ 0x15..=0x19,
                slot,
            } => {
                if *op == 0x19 && is_parameter_null_check(nodes, at) {
                    at += 3;
                    continue;
                }
                if *slot == expected {
                    expected += sizes[argument];
                    argument += 1;
                    at += 1;
                    while matches!(&nodes.get(at), Some(Node::Insn(Insn::Var { op: next, slot: next_slot }))
                        if next == op && next_slot == slot)
                    {
                        at += 1;
                    }
                    continue;
                }
                if *slot < argument_end {
                    return false;
                }
            }
            _ => {}
        }
        at += 1;
    }
    if expected < argument_end {
        return false;
    }
    !nodes[at..].iter().any(|entry| {
        matches!(entry, Node::Insn(Insn::Var { op: 0x15..=0x19 | 0x36..=0x3a, slot } | Insn::Iinc { slot, .. })
            if *slot < argument_end)
    })
}

/// `aload x; ldc "…"; invokestatic Intrinsics.check…` starting at `at` (ASM's `next` steps over
/// labels and lines too, so the three must be adjacent nodes).
fn is_parameter_null_check(nodes: &[Node], at: usize) -> bool {
    matches!(
        (nodes.get(at + 1), nodes.get(at + 2)),
        (
            Some(Node::Insn(Insn::Ldc(_))),
            Some(Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner,
                name,
                desc,
                interface,
            }))
        ) if owner == "kotlin/jvm/internal/Intrinsics"
            && desc == "(Ljava/lang/Object;Ljava/lang/String;)V"
            && !interface
            && matches!(name.as_str(), "checkParameterIsNotNull" | "checkNotNullParameter")
    )
}

/// `opcodeProhibitedDuringArgumentsEvaluation`: jumps, returns, throws, calls, field and array
/// writes, and anything else that can throw or has a side effect.
fn prohibited_during_arguments(insn: &Insn) -> bool {
    let op = match insn {
        Insn::Op(op) | Insn::Int { op, .. } | Insn::Var { op, .. } | Insn::Type { op, .. } => *op,
        Insn::Field { op, .. } | Insn::Method { op, .. } | Insn::Jump { op, .. } => *op,
        Insn::InvokeDynamic { .. } | Insn::MultiANewArray { .. } => return true,
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => return true,
        Insn::Iinc { .. } | Insn::Ldc(_) => return false,
    };
    matches!(
        op,
        0x99..=0xb1
            | 0xc6
            | 0xc7
            | 0xbf
            | PUTSTATIC
            | 0xb5
            | 0xb6..=0xba
            | 0xc2
            | 0xc3
            | 0x6c
            | 0x6d
            | 0x70
            | 0x71
            | 0xc0
            | 0xbc
            | 0xbd
            | 0xc5
            | 0x2e..=0x35
            | 0x4f..=0x56
    )
}
