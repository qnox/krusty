//! Questions the call site asks of a callee body before inlining it.

use std::collections::HashSet;

use crate::jvm::method_node::{Category, Insn, MethodNode, Node};

use super::class_roles::{ClassRoles, RegeneratedClass};
use super::preparation::label_positions;

const ACC_STATIC: u16 = 0x0008;
const GETSTATIC: u8 = 0xb2;
const PUTSTATIC: u8 = 0xb3;
const INVOKESTATIC: u8 = 0xb8;
const NEW: u8 = 0xbb;

/// A body shape the ported stages do not inline yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnsupportedShape {
    /// It constructs a SAM wrapper, which kotlinc regenerates as a `$$inlined$` class for the call
    /// site (`AnonymousObjectTransformer`). An anonymous object is regenerated instead, including
    /// one a `getstatic INSTANCE` loads.
    AnonymousObject,
    /// It reads a `$WhenMappings` table, which kotlinc regenerates for the call site.
    WhenMappings,
    /// It reads `$assertionsDisabled`, a field kotlinc moves to the calling class.
    AssertionsStatus,
}

/// The first shape in `callee` that needs a later stage of the port, if any. The anonymous objects
/// the body constructs are regenerated for the call site; a SAM wrapper is not yet.
pub(crate) fn unsupported_shape(
    callee: &MethodNode,
    classes: &dyn ClassRoles,
) -> Option<UnsupportedShape> {
    let sam_wrapper =
        |internal: &str| classes.regenerated_class(internal) == Some(RegeneratedClass::SamWrapper);
    callee.instructions().find_map(|insn| match insn {
        Insn::Type { op: NEW, class } if sam_wrapper(class) => {
            Some(UnsupportedShape::AnonymousObject)
        }
        Insn::Method { name, owner, .. } if name == "<init>" && sam_wrapper(owner) => {
            Some(UnsupportedShape::AnonymousObject)
        }
        Insn::Field {
            op: GETSTATIC,
            owner,
            name,
            desc,
        } => {
            // `getstatic INSTANCE` of an anonymous object is regenerated with the object, and
            // `needClassReification` is removed once that copy exists. Neither is a shape this
            // gate refuses.
            if name.starts_with("$EnumSwitchMapping$") && owner.ends_with("$WhenMappings") {
                Some(UnsupportedShape::WhenMappings)
            } else if name == "$assertionsDisabled" && desc == "Z" {
                Some(UnsupportedShape::AssertionsStatus)
            } else {
                None
            }
        }
        _ => None,
    })
}

/// Whether `callee` constructs an anonymous object, which the call site regenerates.
pub(crate) fn constructs_anonymous_object(callee: &MethodNode, classes: &dyn ClassRoles) -> bool {
    callee.instructions().any(
        |insn| matches!(insn, Insn::Type { op: NEW, class } if classes.is_anonymous_object(class)),
    )
}

/// `requiresEmptyStackOnEntry` (`IrInlineCodegen`): a body with try/catch blocks, a suspension
/// point (`isBeforeSuspendMarker`/`isBeforeInlineSuspendMarker`) or a backward jump (a loop) is
/// entered with nothing under it on the stack, which kotlinc arranges by spilling the caller's
/// stack.
pub(crate) fn requires_empty_stack_on_entry(callee: &MethodNode) -> bool {
    use crate::jvm::bytecode_passes::coroutines::markers::{
        int_constant, is_suspend_inline_marker, SuspendMarker,
    };
    if !callee.try_catch_blocks.is_empty() {
        return true;
    }
    let before_suspend = |previous: &Node, marker: &Node| {
        is_suspend_inline_marker(marker)
            && matches!(
                int_constant(previous),
                Some(id) if id == SuspendMarker::BeforeSuspend as i32
                    || id == SuspendMarker::BeforeInlineSuspend as i32
            )
    };
    if callee
        .nodes
        .windows(2)
        .any(|pair| before_suspend(&pair[0], &pair[1]))
    {
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

/// Whether the first bytecode the prepared inline body retains is its first parameter load.
///
/// Parameter null checks are removed before the body enters its caller, so they do not count as a
/// prefix. A real instruction before the load does: kotlinc leaves the call-site line off that
/// generated prefix and starts it where the in-place source argument is evaluated.
pub(crate) fn in_place_arguments_begin_at_entry(callee: &MethodNode) -> bool {
    let first_slot = if callee.access & ACC_STATIC == 0 {
        1
    } else {
        0
    };
    let mut at = 0usize;
    while at < callee.nodes.len() {
        match &callee.nodes[at] {
            Node::Label(_) | Node::Line { .. } => at += 1,
            Node::Insn(Insn::Op(0x00)) => at += 1,
            Node::Insn(Insn::Var {
                op: 0x15..=0x19,
                slot,
            }) if *slot == first_slot => {
                if is_parameter_null_check(&callee.nodes, at) {
                    at += 3;
                } else {
                    return true;
                }
            }
            Node::Insn(_) => return false,
        }
    }
    false
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
