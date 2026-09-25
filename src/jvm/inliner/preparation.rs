//! The callee body before it is placed in the caller: kotlinc's `prepareNode`, the fake-variable
//! cleanup, dead-code removal and `removeClosureAssertions`.

use crate::jvm::method_node::{Category, Constant, Insn, LabelId, MethodNode, Node};

use super::{InlineError, Parameters};

const ICONST_0: u8 = 0x03;
const ILOAD: u8 = 0x15;
const ISTORE: u8 = 0x36;
const ALOAD: u8 = 0x19;
const INVOKESTATIC: u8 = 0xb8;

/// `$i$f$…`: the marker variable an inline function's own body declares for the debugger
/// (`JvmAbi.LOCAL_VARIABLE_NAME_PREFIX_INLINE_FUNCTION`), which keeps its name when inlined.
const INLINE_FUNCTION_MARKER_PREFIX: &str = "$i$f$";
/// The suffix an inlined local takes (`INLINE_FUN_VAR_SUFFIX`).
const INLINED_LOCAL_SUFFIX: &str = "$iv";

/// `prepareNode` for a call inlined into ordinary code. The values the call's lambdas capture become
/// parameters after the body's own (`capturedParamsSize` words, which the body's locals move up
/// by). An `@InlineOnly` body loses its line numbers and local variables (nobody steps into it),
/// any other body keeps both, its locals renamed by the old scheme (`x` → `x$iv`, `this` →
/// `this_$iv`).
pub(super) fn prepare(
    callee: &MethodNode,
    inline_only: bool,
    parameters: &Parameters,
) -> MethodNode {
    let mut node = callee.clone();
    add_captured_parameters(&mut node, parameters);
    if inline_only {
        node.nodes
            .retain(|entry| !matches!(entry, Node::Line { .. }));
        node.local_variables.clear();
        return node;
    }
    for local in &mut node.local_variables {
        if local.name.starts_with(INLINE_FUNCTION_MARKER_PREFIX) {
            continue;
        }
        let prefix = if local.name == "this" {
            "this_"
        } else {
            local.name.as_str()
        };
        local.name = format!("{prefix}{INLINED_LOCAL_SUFFIX}");
    }
    node
}

/// Shift every local from the first after the real parameters up by the captured values' words,
/// and declare the captured values as parameters.
fn add_captured_parameters(node: &mut MethodNode, parameters: &Parameters) {
    let shift = parameters.captured_size();
    if shift == 0 {
        return;
    }
    let first = parameters.real_size();
    let moved = |slot: &mut u16| {
        if *slot >= first {
            *slot += shift;
        }
    };
    for entry in &mut node.nodes {
        if let Node::Insn(Insn::Var { slot, .. } | Insn::Iinc { slot, .. }) = entry {
            moved(slot);
        }
    }
    for local in &mut node.local_variables {
        moved(&mut local.slot);
    }
    let close = node
        .desc
        .find(')')
        .expect("a method descriptor has a parameter list");
    let captured: String = parameters
        .captured
        .iter()
        .map(|parameter| match parameter.category {
            Category::Int => "I",
            Category::Float => "F",
            Category::Long => "J",
            Category::Double => "D",
            Category::Reference => "Ljava/lang/Object;",
        })
        .collect();
    node.desc.insert_str(close, &captured);
    node.max_locals += shift;
}

/// `removeFakeVariablesInitializationIfPresent`: before Kotlin 1.6 every inline function began by
/// initializing its marker variable (`iconst_0; istore x`) even when the variable was later dropped
/// (an `@InlineOnly` body loses its variable table). The pair is removed wherever `x` is otherwise
/// unused: never read by `iload` or `iinc`, and not an integral entry of the variable table.
pub(super) fn remove_fake_variable_initializations(node: &mut MethodNode) {
    let mut used = vec![false; node.max_locals as usize];
    let mut mark = |slot: u16| {
        if let Some(entry) = used.get_mut(slot as usize) {
            *entry = true;
        }
    };
    for insn in node.instructions() {
        match insn {
            Insn::Var { op: ILOAD, slot } | Insn::Iinc { slot, .. } => mark(*slot),
            _ => {}
        }
    }
    for local in &node.local_variables {
        if matches!(
            local.desc.as_bytes().first(),
            Some(b'B' | b'C' | b'S' | b'I' | b'Z')
        ) {
            mark(local.slot);
        }
    }
    let mut removed = Vec::new();
    let mut at = 0;
    while at + 2 < node.nodes.len() {
        let pair = (&node.nodes[at], &node.nodes[at + 1], &node.nodes[at + 2]);
        if let (
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var { op: ISTORE, slot }),
            Node::Label(_),
        ) = pair
        {
            if !used.get(*slot as usize).copied().unwrap_or(false) {
                removed.extend([at, at + 1]);
            }
        }
        at += 1;
    }
    if removed.is_empty() {
        return;
    }
    for &index in removed.iter().rev() {
        node.nodes.remove(index);
    }
    remove_empty_try_catch_blocks(node);
}

/// `removeClosureAssertions`: an inlined parameter is never null-checked again. Each
/// `Intrinsics.checkParameterIsNotNull`/`checkNotNullParameter` call goes with the `aload` and
/// `ldc` in front of it.
pub(super) fn remove_closure_assertions(node: &mut MethodNode) -> Result<(), InlineError> {
    let mut removed = Vec::new();
    for (at, entry) in node.nodes.iter().enumerate() {
        let Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner,
            name,
            desc,
            interface,
        }) = entry
        else {
            continue;
        };
        if owner != "kotlin/jvm/internal/Intrinsics"
            || desc != "(Ljava/lang/Object;Ljava/lang/String;)V"
            || *interface
            || !matches!(
                name.as_str(),
                "checkParameterIsNotNull" | "checkNotNullParameter"
            )
        {
            continue;
        }
        let triple = at
            .checked_sub(2)
            .map(|first| (&node.nodes[first], &node.nodes[first + 1]));
        match triple {
            Some((
                Node::Insn(Insn::Var { op: ALOAD, .. }),
                Node::Insn(Insn::Ldc(Constant::String(_))),
            )) => removed.extend([at - 2, at - 1, at]),
            _ => return Err(InlineError::MalformedNullCheck),
        }
    }
    for &index in removed.iter().rev() {
        node.nodes.remove(index);
    }
    Ok(())
}

/// `removeEmptyCatchBlocks`: a try/catch block whose range holds no instruction.
pub(super) fn remove_empty_try_catch_blocks(node: &mut MethodNode) {
    let positions = label_positions(node);
    let nodes = &node.nodes;
    node.try_catch_blocks.retain(|block| {
        let (Some(start), Some(end)) =
            (positions[block.start.index()], positions[block.end.index()])
        else {
            return false;
        };
        start < end
            && nodes[start..end]
                .iter()
                .any(|entry| matches!(entry, Node::Insn(_)))
    });
}

/// Where each label is placed, by [`LabelId::index`].
pub(super) fn label_positions(node: &MethodNode) -> Vec<Option<usize>> {
    let mut positions = vec![None; node.label_count()];
    for (at, entry) in node.nodes.iter().enumerate() {
        if let Node::Label(label) = entry {
            positions[LabelId::index(*label)] = Some(at);
        }
    }
    positions
}
