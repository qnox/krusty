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
use super::frame_store::Frames;
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
) -> Result<Frames<I::V>, AnalyzerError> {
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
) -> Result<Frames<I::V>, AnalyzerError> {
    analyze_kept(method, owner, interpreter, executor, options, |_, _| true)
}

/// [`analyze_with`], keeping the frame only before the nodes `keep` accepts (given each node's
/// index and the node), the merge nodes, and the nodes the walk had to queue. Every other reached
/// node is only marked reached ([`Frames::is_reached`]). A pass that reads the frame at a few
/// kinds of node saves recording a copy of the whole frame before every instruction of the
/// method. The frames it keeps are the ones [`analyze_with`] gives, and the walk visits nodes in
/// the same order, except that it may walk on past a node it does not keep with a frame that node
/// already had, since nothing is left there to compare with.
pub(crate) fn analyze_kept<I: Interpreter, E: Executor<I>>(
    method: &MethodNode,
    owner: &str,
    interpreter: &mut I,
    executor: &mut E,
    options: AnalyzerOptions,
    keep: impl Fn(usize, &Node) -> bool,
) -> Result<Frames<I::V>, AnalyzerError> {
    let count = method.nodes.len();
    if count == 0 {
        return Ok(Frames::new(0));
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
    // The handlers each node reaches, with the type each catches. The type is borrowed from the
    // method: copying it into an owned `String` per covered instruction was pure traffic.
    let mut handlers: Vec<Vec<(usize, &str)>> = vec![Vec::new(); count];
    let mut tcb_start = vec![false; count];
    for block in &method.try_catch_blocks {
        let catch_type = block.catch_type.as_deref().unwrap_or("java/lang/Throwable");
        let handler = positions.at(block.handler);
        let start = positions.at(block.start);
        if start + 1 < count {
            tcb_start[start + 1] = true;
        }
        if options.fast_handlers {
            handlers[start].push((handler, catch_type));
            continue;
        }
        let end = positions.at(block.end);
        for (node, covered) in method.nodes[start..end]
            .iter()
            .zip(&mut handlers[start..end])
        {
            if matches!(node, Node::Insn(_)) {
                covered.push((handler, catch_type));
            }
        }
    }

    let entry = entry_frame(method, owner, interpreter)?;
    let mut work = Worklist {
        frames: Frames::new(count),
        queued: vec![false; count],
        queue: Queue::new(options.in_index_order && !options.fast_merge),
        merge_nodes: merge_nodes(method, &positions),
        fast_merge: options.fast_merge,
    };
    let kept: Vec<bool> = method
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| keep(index, node))
        .collect();
    // Reused for every instruction. Cloning a fresh frame per node allocated the local and stack
    // buffers again at every visit; these two keep their capacity and are copied into the store only
    // when a node's frame is new or changed. The return value never changes, so both keep the
    // entry's.
    let mut scratch = entry.clone();
    let mut handler_frame = entry.clone();
    work.install(0, 0, &entry, interpreter)?;
    while let Some(head) = work.queue.pop() {
        work.queued[head] = false;
        work.frames.copy_into(head, &mut scratch);
        // `scratch` holds the frame before `index`. The walk goes on to the next node while that
        // node is the one the queue would hand out next anyway (see `Worklist::continues_to`).
        let mut index = head;
        loop {
            let insn = match &method.nodes[index] {
                Node::Insn(insn) if opcode(insn) != NOP => Some(insn),
                _ => None,
            };
            let op = insn.map(opcode);
            let reaches_handlers = (!options.prune_exception_edges
                || op.is_some_and(|op| (ISTORE..=ASTORE).contains(&op) || op == IINC)
                || tcb_start[index])
                && !handlers[index].is_empty();
            // Handlers see the frame before this node, not the executed one.
            if reaches_handlers {
                handler_frame.locals.clone_from(&scratch.locals);
            }
            let mut fall_through = None;
            match insn {
                None => fall_through = Some(index + 1),
                Some(insn) => {
                    if opcode(insn) != RETURN {
                        executor.execute(&mut scratch, &At { index, insn }, interpreter)?;
                        // A straight-line instruction has one successor. Building the successor
                        // list allocated on every one of those, which is most of a method.
                        if insn.falls_through()
                            && !matches!(
                                insn,
                                Insn::Jump { .. }
                                    | Insn::TableSwitch { .. }
                                    | Insn::LookupSwitch { .. }
                            )
                        {
                            fall_through = Some(index + 1);
                        } else {
                            let mut targets = successors(insn, index, &positions);
                            if let Some(last) = targets.pop() {
                                for target in targets {
                                    work.edge(index, target, &scratch, interpreter)?;
                                }
                                work.install(index, last, &scratch, interpreter)?;
                            }
                        }
                    }
                }
            }
            let mut next = None;
            if let Some(dest) = fall_through {
                if work.continues_to(dest) {
                    if work.frames.is_recorded(dest) || kept[dest] {
                        if work.install_unqueued(dest, &scratch) {
                            next = Some(dest);
                        }
                    } else {
                        work.frames.mark_reached(dest);
                        next = Some(dest);
                    }
                } else {
                    work.install(index, dest, &scratch, interpreter)?;
                }
            }
            if reaches_handlers {
                for &(handler, catch_type) in &handlers[index] {
                    handler_frame.stack.clear();
                    handler_frame
                        .stack
                        .push(interpreter.new_exception_value(catch_type));
                    work.install(index, handler, &handler_frame, interpreter)?;
                }
            }
            match next {
                // A handler queued below `dest` must still go first.
                Some(dest) if work.queue.lowest().is_none_or(|lowest| lowest > dest) => {
                    index = dest;
                }
                Some(dest) => {
                    // Keep what the walk would have queued.
                    if !work.frames.is_recorded(dest) {
                        work.frames.record(dest, &scratch);
                    }
                    work.queued[dest] = true;
                    work.queue.push(dest);
                    break;
                }
                None => break,
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

    /// The lowest queued index, which an ordered queue hands out next.
    fn lowest(&self) -> Option<usize> {
        match self {
            Queue::Stack(stack) => stack.iter().copied().min(),
            Queue::Ordered(heap) => heap.peek().map(|std::cmp::Reverse(index)| *index),
        }
    }
}

struct Worklist<V> {
    frames: Frames<V>,
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
        self.install(from, to, state, interpreter)
    }

    /// Whether the walk may carry on into `dest` with the frame it holds instead of queueing it:
    /// the queue hands out the lowest index next, and `dest` has the node before it as its only
    /// predecessor, so taking it at once is what the queue would do as long as nothing below it
    /// is queued when the step ends (checked then). A merge node is always installed, since its
    /// frame meets every edge.
    fn continues_to(&self, dest: usize) -> bool {
        matches!(self.queue, Queue::Ordered(_))
            && dest < self.frames.len()
            && !self.merge_nodes[dest]
            && !self.queued[dest]
    }

    /// Store `state` before the non-merge node `dest` the walk continues into, without queueing
    /// it; `true` when the frame there is new or changed, which is when the queue would have
    /// handed `dest` out again.
    fn install_unqueued(&mut self, dest: usize, state: &Frame<V>) -> bool {
        if self.frames.is_recorded(dest) {
            self.frames.replace(dest, state)
        } else {
            self.frames.record(dest, state);
            true
        }
    }

    /// Record `state` as the frame before `dest`. A first arrival copies it into the slot. A
    /// repeat visit of a non-merge node copies only when the frame differs, and does not requeue
    /// an identical one: the old walk treated every repeat visit as a change and ran the rest of
    /// a straight-line block again. `from` is the node the edge leaves, and the index a fall-off
    /// error reports.
    fn install<I: Interpreter<V = V>>(
        &mut self,
        from: usize,
        dest: usize,
        state: &Frame<V>,
        interpreter: &mut I,
    ) -> Result<(), AnalyzerError> {
        if dest >= self.frames.len() {
            return Err(AnalyzerError {
                index: from,
                message: "execution falls off the end of the method".to_string(),
            });
        }
        let is_merge = self.merge_nodes[dest];
        let changed = if !self.frames.is_recorded(dest) {
            self.frames.record(dest, state);
            true
        } else if self.fast_merge {
            false
        } else if !is_merge {
            self.frames.replace(dest, state)
        } else {
            self.frames
                .merge(dest, state, interpreter)
                .map_err(|message| AnalyzerError {
                    index: dest,
                    message,
                })?
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

#[cfg(test)]
mod tests {
    use super::super::{BasicInterpreter, BasicValue};
    use super::*;
    use crate::jvm::method_node::{Insn, MethodNode, Node, TryCatchBlock};

    fn frames_of(method: &MethodNode) -> Frames<BasicValue> {
        analyze(method, "T", &mut BasicInterpreter).expect("the method analyzes")
    }

    #[test]
    fn a_straight_line_store_is_the_frame_before_the_load() {
        let mut method = MethodNode::new(0x0009, "f", "()I");
        method.max_locals = 1;
        method.nodes = vec![
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 0,
            }),
            Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
            Node::Insn(Insn::Op(IRETURN)),
        ];
        let frames = frames_of(&method);
        let before_store = frames.get(1).expect("istore is reached");
        assert_eq!(before_store.locals, vec![BasicValue::Uninitialized]);
        assert_eq!(before_store.stack, vec![BasicValue::Int]);
        let before_load = frames.get(2).expect("iload is reached");
        assert_eq!(before_load.locals, vec![BasicValue::Int]);
        assert_eq!(before_load.stack, Vec::<BasicValue>::new());
        let before_return = frames.get(3).expect("ireturn is reached");
        assert_eq!(before_return.stack, vec![BasicValue::Int]);
    }

    #[test]
    fn both_arms_of_a_branch_meet_as_int_at_the_join() {
        let mut method = MethodNode::new(0x0009, "f", "()I");
        let else_label = method.new_label();
        let join = method.new_label();
        method.nodes = vec![
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Insn(Insn::Jump {
                op: IFEQ,
                target: else_label,
            }),
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Insn(Insn::Jump {
                op: GOTO,
                target: join,
            }),
            Node::Label(else_label),
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Label(join),
            Node::Insn(Insn::Op(IRETURN)),
        ];
        let frames = frames_of(&method);
        let before_return = frames.get(frames.len() - 1).expect("the join is reached");
        assert_eq!(before_return.stack, vec![BasicValue::Int]);
    }

    #[test]
    fn a_loop_back_edge_keeps_the_joined_local_without_revisiting_forever() {
        let mut method = MethodNode::new(0x0009, "f", "()I");
        method.max_locals = 1;
        let head = method.new_label();
        method.nodes = vec![
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 0,
            }),
            Node::Label(head),
            Node::Insn(Insn::Iinc { slot: 0, delta: 1 }),
            Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
            Node::Insn(Insn::Jump {
                op: IFEQ,
                target: head,
            }),
            Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
            Node::Insn(Insn::Op(IRETURN)),
        ];
        let frames = frames_of(&method);
        let before_increment = frames.get(3).expect("the loop body is reached");
        assert_eq!(before_increment.locals, vec![BasicValue::Int]);
        assert_eq!(before_increment.stack, Vec::<BasicValue>::new());
        let before_return = frames.get(7).expect("the exit is reached");
        assert_eq!(before_return.locals, vec![BasicValue::Int]);
        assert_eq!(before_return.stack, vec![BasicValue::Int]);
    }

    /// A loop with a branch inside a protected range: every kind of edge the walk carries on
    /// past or queues.
    fn looping_branch_in_a_handler() -> MethodNode {
        let mut method = MethodNode::new(0x0009, "f", "(I)I");
        method.max_locals = 2;
        let (start, end, handler) = (method.new_label(), method.new_label(), method.new_label());
        let (head, skip) = (method.new_label(), method.new_label());
        method.nodes = vec![
            Node::Label(start),
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 1,
            }),
            Node::Label(head),
            Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
            Node::Insn(Insn::Jump {
                op: IFEQ,
                target: skip,
            }),
            Node::Insn(Insn::Op(ACONST_NULL)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 1,
            }),
            Node::Label(skip),
            Node::Insn(Insn::Iinc { slot: 0, delta: -1 }),
            Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
            Node::Insn(Insn::Jump {
                op: IFNE,
                target: head,
            }),
            Node::Label(end),
            Node::Insn(Insn::Var { op: ILOAD, slot: 0 }),
            Node::Insn(Insn::Op(IRETURN)),
            Node::Label(handler),
            Node::Insn(Insn::Op(POP)),
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Op(IRETURN)),
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Insn(Insn::Op(IRETURN)),
        ];
        method.try_catch_blocks = vec![TryCatchBlock {
            start,
            end,
            handler,
            catch_type: None,
        }];
        method
    }

    #[test]
    fn keeping_only_jumps_keeps_their_frames_and_every_reached_node() {
        let method = looping_branch_in_a_handler();
        let all = frames_of(&method);
        let jumps = analyze_kept(
            &method,
            "T",
            &mut BasicInterpreter,
            &mut PlainFrames,
            AnalyzerOptions::default(),
            |_, node| matches!(node, Node::Insn(Insn::Jump { .. })),
        )
        .expect("the method analyzes");
        assert_ne!(all, jumps, "the sparse analysis leaves frames out");
        for (index, node) in method.nodes.iter().enumerate() {
            assert_eq!(jumps.is_reached(index), all.get(index).is_some(), "{index}");
            if matches!(node, Node::Insn(Insn::Jump { .. })) {
                assert_eq!(jumps.get(index), all.get(index), "{index}");
            }
        }
        assert!(!jumps.is_reached(method.nodes.len() - 1));
    }

    #[test]
    fn keeping_every_frame_is_the_full_analysis() {
        let method = looping_branch_in_a_handler();
        let kept = analyze_kept(
            &method,
            "T",
            &mut BasicInterpreter,
            &mut PlainFrames,
            AnalyzerOptions::default(),
            |_, _| true,
        )
        .expect("the method analyzes");
        let at_merge = kept.get(3).expect("the loop head is reached");
        assert_eq!(
            at_merge.locals,
            vec![BasicValue::Int, BasicValue::Uninitialized]
        );
        assert_eq!(kept, frames_of(&method));
    }

    #[test]
    #[should_panic(expected = "did not keep the frame")]
    fn reading_a_frame_the_analysis_did_not_keep_is_a_bug() {
        let method = looping_branch_in_a_handler();
        let none = analyze_kept(
            &method,
            "T",
            &mut BasicInterpreter,
            &mut PlainFrames,
            AnalyzerOptions::default(),
            |_, _| false,
        )
        .expect("the method analyzes");
        none.get(2);
    }

    #[test]
    fn a_handler_sees_the_frame_from_before_the_throwing_instruction() {
        let mut method = MethodNode::new(0x0009, "f", "()V");
        method.max_locals = 1;
        let start = method.new_label();
        let end = method.new_label();
        let handler = method.new_label();
        method.nodes = vec![
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 0,
            }),
            Node::Label(start),
            Node::Insn(Insn::Op(ACONST_NULL)),
            Node::Insn(Insn::Op(ATHROW)),
            Node::Label(end),
            Node::Label(handler),
            Node::Insn(Insn::Op(RETURN)),
        ];
        method.try_catch_blocks = vec![TryCatchBlock {
            start,
            end,
            handler,
            catch_type: Some("java/lang/Throwable".to_string()),
        }];
        let frames = frames_of(&method);
        let at_handler = frames.get(6).expect("the handler is reached");
        assert_eq!(at_handler.locals, vec![BasicValue::Int]);
        assert_eq!(
            at_handler.stack,
            vec![BasicValue::reference("Ljava/lang/Throwable;")]
        );
    }
}
