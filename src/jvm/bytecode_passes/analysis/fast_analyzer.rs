//! kotlinc's `FastMethodAnalyzer` (`codegen/optimization/common/FastAnalyzer.kt`), the forward
//! analyzer every kotlinc bytecode pass runs its interpreters with. It differs from ASM's `Analyzer`
//! in ways a port must keep, because they decide which values an analysis reports:
//!
//! - a node that is not a jump target or handler entry takes the arriving frame as it is, rather
//!   than merging it with what it held;
//! - an exception edge carries the frame BEFORE the instruction it leaves, never after;
//! - `return` is not executed, so a mistyped return value is never reported.

use super::super::descriptors;
use super::super::opcodes::*;
use super::frame::{At, Frame, Interpreter};
use super::opcode;
use crate::jvm::method_node::LabelPositions;
use crate::jvm::method_node::{Insn, MethodNode, Node};

/// Why an analysis could not complete: the node it stopped at and what was wrong there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnalyzerError {
    pub index: usize,
    pub message: String,
}

const ACC_STATIC: u16 = 0x0008;

/// The variations of kotlinc's `FastAnalyzer` its analyzers choose between.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AnalyzerOptions {
    /// `pruneExceptionEdges`: only stores, `iinc` and the first node of a protected range reach a
    /// handler.
    pub prune_exception_edges: bool,
    /// `useFastComputeExceptionHandlers`: a handler is reached only from its range's start label.
    pub fast_handlers: bool,
    /// `useFastMergeControlFlowEdge`: a node keeps the first frame that reaches it.
    pub fast_merge: bool,
    /// Take the queued node with the lowest index next instead of the last one queued; on by
    /// default. Without `fast_merge` the frames an analysis reaches do not depend on the order it
    /// visits nodes in, only how often it revisits them: last-in-first-out walks the rest of a
    /// method again after every branch that joins it, and after every node whose frame widens a
    /// handler that covers much of the method. A large data class's `copy$default`, or a
    /// suspend lambda that inlines many calls with `try` blocks, cannot afford that.
    pub in_index_order: bool,
}

impl Default for AnalyzerOptions {
    fn default() -> Self {
        AnalyzerOptions {
            prune_exception_edges: false,
            fast_handlers: false,
            fast_merge: false,
            in_index_order: true,
        }
    }
}

/// How a frame executes one instruction: ASM's `Frame.execute` unless an analysis's own frame
/// class overrides it.
pub(crate) trait Executor<I: Interpreter> {
    fn execute(
        &mut self,
        frame: &mut Frame<I::V>,
        at: &At,
        interpreter: &mut I,
    ) -> Result<(), AnalyzerError> {
        frame.execute(at, interpreter)
    }
}

/// ASM's plain `Frame`.
pub(crate) struct PlainFrames;

impl<I: Interpreter> Executor<I> for PlainFrames {}

/// The frame before every reachable node of `method`, a member of `owner` (the type of `this`),
/// and `None` where no path reaches: `FastMethodAnalyzer` with ASM's frames.
pub(crate) fn analyze<I: Interpreter>(
    method: &MethodNode,
    owner: &str,
    interpreter: &mut I,
) -> Result<Vec<Option<Frame<I::V>>>, AnalyzerError> {
    analyze_with(
        method,
        owner,
        interpreter,
        &mut PlainFrames,
        AnalyzerOptions::default(),
    )
}

/// kotlinc's `FastAnalyzer` with the given options and frame behaviour.
pub(crate) fn analyze_with<I: Interpreter, E: Executor<I>>(
    method: &MethodNode,
    owner: &str,
    interpreter: &mut I,
    executor: &mut E,
    options: AnalyzerOptions,
) -> Result<Vec<Option<Frame<I::V>>>, AnalyzerError> {
    let count = method.nodes.len();
    if count == 0 {
        return Ok(Vec::new());
    }
    let positions = LabelPositions::of(method);
    if method
        .nodes
        .iter()
        .any(|node| matches!(node, Node::Insn(insn) if matches!(opcode(insn), JSR | RET)))
    {
        return Err(AnalyzerError {
            index: 0,
            message: "subroutines are not supported".to_string(),
        });
    }
    // The handlers each node reaches, with the type each catches.
    let mut handlers: Vec<Vec<(usize, String)>> = vec![Vec::new(); count];
    let mut tcb_start = vec![false; count];
    for block in &method.try_catch_blocks {
        let catch_type = block.catch_type.as_deref().unwrap_or("java/lang/Throwable");
        let handler = positions.at(block.handler);
        let start = positions.at(block.start);
        if start + 1 < count {
            tcb_start[start + 1] = true;
        }
        if options.fast_handlers {
            handlers[start].push((handler, catch_type.to_string()));
            continue;
        }
        let end = positions.at(block.end);
        for (node, covered) in method.nodes[start..end]
            .iter()
            .zip(&mut handlers[start..end])
        {
            if matches!(node, Node::Insn(_)) {
                covered.push((handler, catch_type.to_string()));
            }
        }
    }

    let entry = entry_frame(method, owner, interpreter)?;
    let mut work = Worklist {
        frames: vec![None; count],
        queued: vec![false; count],
        queue: Queue::new(options.in_index_order && !options.fast_merge),
        merge_nodes: merge_nodes(method, &positions),
        fast_merge: options.fast_merge,
    };
    work.merge(0, &entry, interpreter)?;
    while let Some(index) = work.queue.pop() {
        work.queued[index] = false;
        let before = work.frames[index]
            .clone()
            .expect("a queued node has a frame");
        let insn = match &method.nodes[index] {
            Node::Insn(insn) if opcode(insn) != NOP => Some(insn),
            _ => None,
        };
        let op = insn.map(opcode);
        match insn {
            None => work.edge(index, index + 1, &before, interpreter)?,
            Some(insn) => {
                let mut current = before.clone();
                if opcode(insn) != RETURN {
                    executor.execute(&mut current, &At { index, insn }, interpreter)?;
                }
                for target in successors(insn, index, &positions) {
                    work.edge(index, target, &current, interpreter)?;
                }
            }
        }
        let reaches_handlers = !options.prune_exception_edges
            || op.is_some_and(|op| (ISTORE..=ASTORE).contains(&op) || op == IINC)
            || tcb_start[index];
        if reaches_handlers {
            for (handler, catch_type) in &handlers[index] {
                let mut state = before.clone();
                state.stack.clear();
                state
                    .stack
                    .push(interpreter.new_exception_value(catch_type));
                work.merge(*handler, &state, interpreter)?;
            }
        }
    }
    Ok(work.frames)
}

/// Where control goes after `insn`, in the order kotlinc's analyzer queues the edges.
fn successors(insn: &Insn, index: usize, positions: &LabelPositions) -> Vec<usize> {
    let mut targets = Vec::new();
    if insn.falls_through() {
        targets.push(index + 1);
    }
    let labels = insn.jump_targets();
    match insn {
        // kotlinc visits a table switch's cases reversed, after its default.
        Insn::TableSwitch { .. } => {
            targets.push(positions.at(labels[0]));
            targets.extend(labels[1..].iter().rev().map(|label| positions.at(*label)));
        }
        _ => targets.extend(labels.iter().map(|label| positions.at(*label))),
    }
    targets
}

/// The nodes waiting to be visited: a stack, or, `in_index_order`, a min-heap by node index.
enum Queue {
    Stack(Vec<usize>),
    Ordered(std::collections::BinaryHeap<std::cmp::Reverse<usize>>),
}

impl Queue {
    fn new(in_index_order: bool) -> Queue {
        if in_index_order {
            Queue::Ordered(std::collections::BinaryHeap::new())
        } else {
            Queue::Stack(Vec::new())
        }
    }

    fn push(&mut self, index: usize) {
        match self {
            Queue::Stack(stack) => stack.push(index),
            Queue::Ordered(heap) => heap.push(std::cmp::Reverse(index)),
        }
    }

    fn pop(&mut self) -> Option<usize> {
        match self {
            Queue::Stack(stack) => stack.pop(),
            Queue::Ordered(heap) => heap.pop().map(|std::cmp::Reverse(index)| index),
        }
    }
}

struct Worklist<V> {
    frames: Vec<Option<Frame<V>>>,
    queued: Vec<bool>,
    queue: Queue,
    merge_nodes: Vec<bool>,
    fast_merge: bool,
}

impl<V: super::frame::Value> Worklist<V> {
    fn edge<I: Interpreter<V = V>>(
        &mut self,
        from: usize,
        to: usize,
        state: &Frame<V>,
        interpreter: &mut I,
    ) -> Result<(), AnalyzerError> {
        if to >= self.frames.len() {
            return Err(AnalyzerError {
                index: from,
                message: "execution falls off the end of the method".to_string(),
            });
        }
        self.merge(to, state, interpreter)
    }

    fn merge<I: Interpreter<V = V>>(
        &mut self,
        dest: usize,
        state: &Frame<V>,
        interpreter: &mut I,
    ) -> Result<(), AnalyzerError> {
        let is_merge = self.merge_nodes[dest];
        let changed = match &mut self.frames[dest] {
            slot @ None => {
                *slot = Some(state.clone());
                true
            }
            Some(_) if self.fast_merge => false,
            Some(old) if !is_merge => {
                *old = state.clone();
                true
            }
            Some(old) => old
                .merge(state, interpreter)
                .map_err(|message| AnalyzerError {
                    index: dest,
                    message,
                })?,
        };
        if changed && !self.queued[dest] {
            self.queued[dest] = true;
            self.queue.push(dest);
        }
        Ok(())
    }
}
/// Jump and switch targets and handler entries: the nodes whose frame is a merge.
fn merge_nodes(method: &MethodNode, positions: &LabelPositions) -> Vec<bool> {
    let mut merge = vec![false; method.nodes.len()];
    for node in &method.nodes {
        if let Node::Insn(insn) = node {
            for label in insn.jump_targets() {
                merge[positions.at(label)] = true;
            }
        }
    }
    for block in &method.try_catch_blocks {
        merge[positions.at(block.handler)] = true;
    }
    merge
}

fn entry_frame<I: Interpreter>(
    method: &MethodNode,
    owner: &str,
    interpreter: &mut I,
) -> Result<Frame<I::V>, AnalyzerError> {
    let malformed = || AnalyzerError {
        index: 0,
        message: format!("malformed method descriptor {}", method.desc),
    };
    let return_type = descriptors::return_type(&method.desc).ok_or_else(malformed)?;
    let mut locals = Vec::with_capacity(usize::from(method.max_locals));
    if method.access & ACC_STATIC == 0 {
        locals.push(interpreter.new_parameter_value(0, &descriptors::of_internal_name(owner)));
    }
    for argument in descriptors::argument_types(&method.desc).ok_or_else(malformed)? {
        locals.push(interpreter.new_parameter_value(locals.len(), argument));
        if descriptors::size(argument) == 2 {
            locals.push(interpreter.new_empty_value());
        }
    }
    if locals.len() > usize::from(method.max_locals) {
        return Err(AnalyzerError {
            index: 0,
            message: "the parameters exceed max_locals".to_string(),
        });
    }
    while locals.len() < usize::from(method.max_locals) {
        locals.push(interpreter.new_empty_value());
    }
    Ok(Frame {
        locals,
        stack: Vec::new(),
        return_value: interpreter.new_value(Some(return_type)),
    })
}
