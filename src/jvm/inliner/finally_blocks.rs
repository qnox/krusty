//! kotlinc's `InternalFinallyBlockInliner.processInlineFunFinallyBlocks`, which runs on the root
//! inlined body once its lambdas are expanded and its locals moved into the caller's frame.
//!
//! A lambda's non-local `return` leaves every `try` of the inlined function it stands in, so each
//! such `finally` must run before it. The function's own body cannot do that: its compiled
//! `finally` copies only cover its own exits. kotlinc therefore copies the `finally` body in front
//! of each marked return (see `returns`), innermost first: it finds the body between the
//! `InlineMarker.finallyStart`/`finallyEnd` calls with the depth of that `try`, parks the returned
//! value in a fresh local around the copy, and splits every protected range and local variable that
//! covered the return so the copy is not covered by the handlers its own `try` has already left.
//!
//! The body is walked from its end, so that the copies placed before a return are complete before
//! an earlier return in the same `finally` is reached. Each `try`/`catch` block and each local
//! variable is an interval between two labels, and the walk keeps the set of those covering the
//! current node (`CoveringTryCatchNodeProcessor`). The first `lambda_blocks` try/catch blocks come
//! from the expanded lambdas: they are copied but never searched for a `finally`.
//!
//! When the function has a `finally` and no lambda returns non-locally, only the closing step
//! remains: the blocks are ordered by handler, then start, then end, and blocks and variables that
//! cover no instruction are dropped (`substituteTryBlockNodes`, `substituteLocalVarTable`).

use crate::fx_hash::{HashMap, HashSet};
use crate::jvm::bytecode_passes::coroutines::markers::{
    int_constant, is_finally_marker, INLINE_MARKER_CLASS,
};
use crate::jvm::method_node::{Constant, Insn, LabelId, LocalVariable, MethodNode, Node};

use super::preparation::label_positions;
use super::returns::is_marked_return;
use super::InlineError;

const INVOKESTATIC: u8 = 0xb8;
const RETURN: u8 = 0xb1;
const ACC_STATIC: u16 = 0x0008;

/// One protected range of the body (`TryCatchBlockNodeInfo`).
#[derive(Clone)]
struct TryBlock {
    start: LabelId,
    end: LabelId,
    handler: LabelId,
    catch_type: Option<String>,
    /// The block came from an inlined lambda: it is copied with a `finally`, never searched for one.
    only_copy: bool,
}

/// An interval between two labels that a copied `finally` can split.
trait Interval: Clone {
    fn start(&self) -> LabelId;
    fn end(&self) -> LabelId;
    fn set_start(&mut self, start: LabelId);
    fn set_end(&mut self, end: LabelId);

    fn with_bounds(&self, start: LabelId, end: LabelId) -> Self {
        let mut copy = self.clone();
        copy.set_start(start);
        copy.set_end(end);
        copy
    }
}

impl Interval for TryBlock {
    fn start(&self) -> LabelId {
        self.start
    }
    fn end(&self) -> LabelId {
        self.end
    }
    fn set_start(&mut self, start: LabelId) {
        self.start = start;
    }
    fn set_end(&mut self, end: LabelId) {
        self.end = end;
    }
}

impl Interval for LocalVariable {
    fn start(&self) -> LabelId {
        self.start
    }
    fn end(&self) -> LabelId {
        self.end
    }
    fn set_start(&mut self, start: LabelId) {
        self.start = start;
    }
    fn set_end(&mut self, end: LabelId) {
        self.end = end;
    }
}

/// kotlinc's `IntervalMetaInfo`: every interval, by the labels it starts and ends at, and the ones
/// covering the node the walk stands at. Intervals are named by their index in `intervals`;
/// `order` is kotlinc's `allIntervals` list, which sorting reorders.
struct Intervals<T> {
    intervals: Vec<T>,
    order: Vec<usize>,
    starts: HashMap<LabelId, Vec<usize>>,
    ends: HashMap<LabelId, Vec<usize>>,
    current: Vec<usize>,
}

impl<T: Interval> Intervals<T> {
    fn new(intervals: impl IntoIterator<Item = T>) -> Self {
        let mut all = Intervals {
            intervals: Vec::new(),
            order: Vec::new(),
            starts: HashMap::default(),
            ends: HashMap::default(),
            current: Vec::new(),
        };
        for interval in intervals {
            all.add(interval);
        }
        all
    }

    fn add(&mut self, interval: T) -> usize {
        let id = self.intervals.len();
        self.starts.entry(interval.start()).or_default().push(id);
        self.ends.entry(interval.end()).or_default().push(id);
        self.intervals.push(interval);
        self.order.push(id);
        id
    }

    fn starting_at(&self, label: LabelId) -> &[usize] {
        self.starts.get(&label).map_or(&[], Vec::as_slice)
    }

    fn ending_at(&self, label: LabelId) -> &[usize] {
        self.ends.get(&label).map_or(&[], Vec::as_slice)
    }

    /// `processCurrent` walking backwards: the intervals ending at `label` start covering, the
    /// ones starting at it stop.
    fn pass_backwards(&mut self, label: LabelId) -> Result<(), InlineError> {
        for id in self.ending_at(label).to_vec() {
            if self.current.contains(&id) {
                return Err(malformed("an interval is entered twice"));
            }
            self.current.push(id);
        }
        for id in self.starting_at(label).to_vec() {
            let Some(at) = self.current.iter().position(|&known| known == id) else {
                return Err(malformed("an interval is left before it was entered"));
            };
            self.current.remove(at);
        }
        Ok(())
    }

    /// `split`: cut `[by_start, by_end)` out of interval `id`. With `keep_start` the interval keeps
    /// its start and ends at `by_start`; otherwise it starts again at `by_end`. The other part is
    /// a new interval.
    fn split(&mut self, id: usize, by_start: LabelId, by_end: LabelId, keep_start: bool) -> usize {
        let interval = &mut self.intervals[id];
        let new_part = if keep_start {
            let old_end = interval.end();
            interval.set_end(by_start);
            let new_part = interval.with_bounds(by_end, old_end);
            remove_entry(&mut self.ends, old_end, id);
            self.ends.entry(by_start).or_default().push(id);
            new_part
        } else {
            let old_start = interval.start();
            interval.set_start(by_end);
            let new_part = interval.with_bounds(old_start, by_start);
            remove_entry(&mut self.starts, old_start, id);
            self.starts.entry(by_end).or_default().push(id);
            new_part
        };
        self.add(new_part)
    }

    /// `splitAndRemoveIntervalFromCurrents`.
    fn split_and_leave(
        &mut self,
        id: usize,
        by_start: LabelId,
        by_end: LabelId,
        keep_start: bool,
    ) -> Result<(), InlineError> {
        self.split(id, by_start, by_end, keep_start);
        let Some(at) = self.current.iter().position(|&known| known == id) else {
            return Err(malformed("a split interval does not cover the return"));
        };
        self.current.remove(at);
        Ok(())
    }

    /// `copyIntervalsForRange`: the part of each interval inside `[start, end)`.
    fn copies_within(&self, positions: &[Option<usize>], start: LabelId, end: LabelId) -> Vec<T> {
        let at = |label: LabelId| positions[label.index()];
        self.order
            .iter()
            .filter_map(|&id| {
                let interval = &self.intervals[id];
                let (from, to) = (at(interval.start())?, at(interval.end())?);
                let (other_from, other_to) = (at(start)?, at(end)?);
                let (new_from, new_to) = (from.max(other_from), to.min(other_to));
                if new_from >= new_to {
                    return None;
                }
                let new_start = if from == new_from {
                    interval.start()
                } else {
                    start
                };
                let new_end = if to == new_to { interval.end() } else { end };
                Some(interval.with_bounds(new_start, new_end))
            })
            .collect()
    }

    /// The intervals that cover an instruction, in `order` (`getMeaningfulIntervals`).
    fn meaningful(&self, nodes: &[Node], positions: &[Option<usize>]) -> Vec<T> {
        self.order
            .iter()
            .map(|&id| &self.intervals[id])
            .filter(|interval| {
                covers_an_instruction(nodes, positions, interval.start(), interval.end())
            })
            .cloned()
            .collect()
    }
}

fn remove_entry(map: &mut HashMap<LabelId, Vec<usize>>, label: LabelId, id: usize) {
    if let Some(ids) = map.get_mut(&label) {
        if let Some(at) = ids.iter().position(|&known| known == id) {
            ids.remove(at);
        }
    }
}

fn malformed(reason: &str) -> InlineError {
    InlineError::Analysis(format!("finally blocks: {reason}"))
}

/// Whether an instruction lies in `[start, end)` (the opposite of kotlinc's `isMeaningless`).
fn covers_an_instruction(
    nodes: &[Node],
    positions: &[Option<usize>],
    start: LabelId,
    end: LabelId,
) -> bool {
    let (Some(from), Some(to)) = (positions[start.index()], positions[end.index()]) else {
        return false;
    };
    from < to
        && nodes[from..to]
            .iter()
            .any(|node| matches!(node, Node::Insn(_)))
}

/// `firstLabelInChain`: the first of the labels placed directly before the node `label` names.
fn first_label_in_chain(nodes: &[Node], positions: &[Option<usize>], label: LabelId) -> LabelId {
    let Some(mut at) = positions[label.index()] else {
        return label;
    };
    let mut first = label;
    while let Some(before) = at.checked_sub(1) {
        let Node::Label(previous) = nodes[before] else {
            break;
        };
        first = previous;
        at = before;
    }
    first
}

/// `getLineNumberOrNull`: the line of the nearest line number at or before `at`.
fn line_at(nodes: &[Node], at: usize) -> Option<u16> {
    nodes[..=at].iter().rev().find_map(|node| match node {
        Node::Line { line, .. } => Some(*line),
        _ => None,
    })
}

/// `getFirstFinallyOperationLineNumberOrNull`: the line of the first instruction in
/// `[from, to)` that is not a `finally` marker or its depth.
fn first_operation_line(nodes: &[Node], from: usize, to: usize) -> Option<u16> {
    let mut at = from;
    while at < to {
        if !matches!(nodes[at], Node::Insn(_)) {
            at += 1;
        } else if nodes.get(at + 1).is_some_and(is_finally_marker) {
            at += 2;
        } else {
            return line_at(nodes, at);
        }
    }
    None
}

fn is_finally_end(node: &Node) -> bool {
    matches!(node, Node::Insn(Insn::Method { op: INVOKESTATIC, owner, name, .. })
        if owner == INLINE_MARKER_CLASS && name == "finallyEnd")
}

/// Where a `finally` body lies (`FinallyBlockInfo`): node indices of its first node and of the node
/// after it, and the labels just before and after it.
struct FinallyBody {
    start: usize,
    end_exclusive: usize,
    label_before: LabelId,
    label_after: LabelId,
}

impl FinallyBody {
    /// kotlinc's `isEmpty`, which looks only at whether the body is a run of labels.
    fn is_empty(&self, nodes: &[Node]) -> bool {
        if !matches!(nodes[self.start], Node::Label(_)) {
            return false;
        }
        let mut end = self.end_exclusive;
        while end != self.start && matches!(nodes[end], Node::Label(_)) {
            end -= 1;
        }
        end == self.start
    }

    fn shift(&mut self, from: usize, by: usize) {
        if self.start >= from {
            self.start += by;
        }
        if self.end_exclusive >= from {
            self.end_exclusive += by;
        }
    }
}

/// Where a try/catch block found in a `finally` body stands relative to it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Start,
    End,
    Inner,
}

/// The value a return leaves, by its opcode: its store and load, and their words.
fn return_value(op: u8) -> Option<(u8, u8, u16)> {
    match op {
        0xac => Some((0x36, 0x15, 1)),
        0xad => Some((0x37, 0x16, 2)),
        0xae => Some((0x38, 0x17, 1)),
        0xaf => Some((0x39, 0x18, 2)),
        0xb0 => Some((0x3a, 0x19, 1)),
        _ => None,
    }
}

/// The slots the receiver and arguments of `node` take (ASM's `getArgumentsAndReturnSizes`).
fn arguments_words(node: &MethodNode) -> u16 {
    let mut words = u16::from(node.access & ACC_STATIC == 0);
    let arguments = node.desc[1..]
        .split(')')
        .next()
        .unwrap_or_default()
        .as_bytes();
    let mut at = 0;
    while at < arguments.len() {
        let start = at;
        while arguments[at] == b'[' {
            at += 1;
        }
        if arguments[at] == b'L' {
            while arguments[at] != b';' {
                at += 1;
            }
        }
        words += if at == start && matches!(arguments[at], b'J' | b'D') {
            2
        } else {
            1
        };
        at += 1;
    }
    words
}

/// Copy the inlined function's `finally` blocks in front of each non-local return of the root
/// inlined `node`, whose first `lambda_blocks` try/catch blocks come from its lambdas.
pub(super) fn inline_finally_blocks(
    node: &mut MethodNode,
    lambda_blocks: usize,
) -> Result<(), InlineError> {
    let blocks: Vec<TryBlock> = node
        .try_catch_blocks
        .iter()
        .enumerate()
        .map(|(index, block)| TryBlock {
            start: block.start,
            end: block.end,
            handler: block.handler,
            catch_type: block.catch_type.clone(),
            only_copy: index < lambda_blocks,
        })
        .collect();
    if !blocks
        .iter()
        .any(|block| !block.only_copy && block.catch_type.is_none())
    {
        return Ok(());
    }
    let mut inliner = FinallyInliner {
        tries: Intervals::new(blocks),
        locals: Intervals::new(node.local_variables.iter().cloned()),
        // kotlinc's `MaxLocalsCalculator` over the body: the first slot nothing in it uses.
        next_value_slot: node.locals_top().max(arguments_words(node)),
    };
    inliner.walk(node)?;
    inliner.sort_tries(node, None);
    let positions = label_positions(node);
    node.try_catch_blocks = inliner
        .tries
        .meaningful(&node.nodes, &positions)
        .into_iter()
        .map(|block| crate::jvm::method_node::TryCatchBlock {
            start: block.start,
            end: block.end,
            handler: block.handler,
            catch_type: block.catch_type,
        })
        .collect();
    node.local_variables = inliner.locals.meaningful(&node.nodes, &positions);
    Ok(())
}

struct FinallyInliner {
    tries: Intervals<TryBlock>,
    locals: Intervals<LocalVariable>,
    /// The slot the next returned value is parked in while a copy runs.
    next_value_slot: u16,
}

impl FinallyInliner {
    /// `processInstruction` walking backwards.
    fn pass(&mut self, node: &Node) -> Result<(), InlineError> {
        if let Node::Label(label) = node {
            self.tries.pass_backwards(*label)?;
            self.locals.pass_backwards(*label)?;
        }
        Ok(())
    }

    /// `sortTryCatchBlocks`: by handler, then start, then end. With `ids` the given blocks are
    /// sorted and returned; otherwise the whole `order` list is sorted in place.
    fn sort_tries(&mut self, node: &MethodNode, ids: Option<Vec<usize>>) -> Vec<usize> {
        let positions = label_positions(node);
        let at = |label: LabelId| positions[label.index()];
        let key = |id: &usize| {
            let block = &self.tries.intervals[*id];
            (at(block.handler), at(block.start), at(block.end))
        };
        match ids {
            Some(mut ids) => {
                ids.sort_by_key(key);
                ids
            }
            None => {
                let mut order = std::mem::take(&mut self.tries.order);
                order.sort_by_key(key);
                self.tries.order = order;
                Vec::new()
            }
        }
    }

    fn walk(&mut self, node: &mut MethodNode) -> Result<(), InlineError> {
        let mut cursor = node.nodes.len().checked_sub(1);
        while let Some(at) = cursor {
            self.pass(&node.nodes[at].clone())?;
            if !is_marked_return(&node.nodes, at) {
                cursor = at.checked_sub(1);
                continue;
            }
            let covering = self.sort_tries(node, Some(self.tries.current.clone()));
            match covering.last() {
                None => {
                    cursor = at.checked_sub(1);
                    continue;
                }
                Some(&outermost) if self.tries.intervals[outermost].only_copy => {
                    cursor = at.checked_sub(1);
                    continue;
                }
                Some(_) => {}
            }
            cursor = self.copy_finally_blocks(node, at, &covering)?;
        }
        Ok(())
    }

    /// Copy every `finally` covering the marked return at `at` in front of it; returns where the
    /// walk goes on.
    fn copy_finally_blocks(
        &mut self,
        node: &mut MethodNode,
        at: usize,
        covering: &[usize],
    ) -> Result<Option<usize>, InlineError> {
        let mut marker = at - 1;
        let before_marker = marker.checked_sub(1);
        let Some(&Node::Label(new_finally_end)) = node.nodes.get(at + 1) else {
            return Err(malformed("a non-local return is not followed by a label"));
        };
        let line_after_return = (at + 2 < node.nodes.len())
            .then(|| line_at(&node.nodes, at + 2))
            .flatten();
        let Node::Insn(Insn::Op(return_op)) = node.nodes[at] else {
            unreachable!("a marked return is a return instruction");
        };
        let value = return_value(return_op);
        debug_assert!(value.is_some() || return_op == RETURN);

        let mut depth_index = 0;
        let mut nested_unsplit_without_finally: Vec<usize> = Vec::new();
        let mut new_local_intervals: Vec<LocalVariable> = Vec::new();
        for cluster in self.clusters(node, covering) {
            let with_default_handler = *cluster.last().expect("a cluster has a block");
            let Some(mut body) = self.finally_body(node, with_default_handler)? else {
                nested_unsplit_without_finally.extend(&cluster);
                continue;
            };
            if self.tries.intervals[with_default_handler].only_copy {
                return Err(malformed("a lambda's try block has a finally to copy"));
            }
            depth_index += 1;
            let in_finally = self.tries_in_finally(&node.nodes, &body);

            let new_finally_start = node.new_label();
            let inserted_block_end = node.new_label();
            let park = value.filter(|_| !body.is_empty(&node.nodes));
            let mut copy = Vec::new();
            if let Some((store, _, _)) = park {
                copy.push(Node::Insn(Insn::Var {
                    op: store,
                    slot: self.next_value_slot,
                }));
            }
            copy.push(Node::Label(new_finally_start));
            let labels_inside: Vec<LabelId> = node.nodes[body.start..body.end_exclusive]
                .iter()
                .filter_map(|entry| match entry {
                    Node::Label(label) => Some(*label),
                    _ => None,
                })
                .collect();
            let positions = label_positions(node);
            let mut vars_inside =
                self.locals
                    .copies_within(&positions, body.label_before, body.label_after);
            let mut old_to_new: HashMap<LabelId, LabelId> = HashMap::default();
            for &label in &labels_inside {
                let new = node.new_label();
                old_to_new.insert(label, new);
            }
            let inside: HashSet<LabelId> = labels_inside.iter().copied().collect();
            let map = |label: LabelId| old_to_new.get(&label).copied().unwrap_or(label);
            for index in body.start..body.end_exclusive {
                let entry = &node.nodes[index];
                let copied = match entry {
                    Node::Label(label) => Node::Label(map(*label)),
                    Node::Line { line, start } => Node::Line {
                        line: *line,
                        start: map(*start),
                    },
                    Node::Insn(insn) => {
                        let depth = int_constant(entry)
                            .filter(|_| node.nodes.get(index + 1).is_some_and(is_finally_marker));
                        match depth {
                            Some(depth) => {
                                Node::Insn(Insn::Ldc(Constant::Int(depth + depth_index)))
                            }
                            None => Node::Insn(relabel(insn, &inside, &map)),
                        }
                    }
                };
                copy.push(copied);
            }
            if let Some((_, load, words)) = park {
                copy.push(Node::Insn(Insn::Var {
                    op: load,
                    slot: self.next_value_slot,
                }));
                self.next_value_slot += words;
            }
            copy.push(Node::Label(inserted_block_end));
            for var in &mut vars_inside {
                remap_label(var, body.label_before, new_finally_start);
                for (&old, &new) in &old_to_new {
                    remap_label(var, old, new);
                }
                remap_label(var, body.label_after, inserted_block_end);
            }
            new_local_intervals.extend(vars_inside);

            let copy_start = marker;
            let copied = copy.len();
            node.nodes.splice(copy_start..copy_start, copy);
            marker += copied;
            body.shift(copy_start, copied);

            let original_line = first_operation_line(&node.nodes, body.start, body.end_exclusive);
            let copy_line = first_operation_line(&node.nodes, copy_start, marker);
            if let Some(line) = original_line.filter(|&line| Some(line) != copy_line) {
                let label = node.new_label();
                node.nodes.splice(
                    copy_start..copy_start,
                    [Node::Label(label), Node::Line { line, start: label }],
                );
                marker += 2;
            }

            nested_unsplit_without_finally.extend(&cluster);
            self.update_exception_table(
                node,
                &nested_unsplit_without_finally,
                (new_finally_start, new_finally_end),
                &in_finally,
                &old_to_new,
                inserted_block_end,
            )?;
            nested_unsplit_without_finally.clear();
        }

        let mut return_at = marker + 1;
        let after_return = return_at + 2;
        if let Some(line) = line_after_return {
            if after_return < node.nodes.len() && line_at(&node.nodes, after_return) != Some(line) {
                let label = node.new_label();
                node.nodes.splice(
                    after_return..after_return,
                    [Node::Label(label), Node::Line { line, start: label }],
                );
            }
        }

        // Step over the copies just placed.
        let mut cursor = Some(marker);
        while let Some(index) = cursor {
            if Some(index) == before_marker {
                break;
            }
            self.pass(&node.nodes[index].clone())?;
            cursor = index.checked_sub(1);
        }
        let inserted = before_marker.map_or(marker, |before| marker - before - 1);
        if let (true, Some(before)) = (inserted > 0, cursor) {
            // The return splits each variable covering it around the copies.
            let start = node.new_label();
            let end = node.new_label();
            node.nodes.insert(before + 1, Node::Label(start));
            return_at += 1;
            node.nodes.insert(return_at + 1, Node::Label(end));
            for id in self.locals.current.clone() {
                self.locals.split(id, start, end, true);
            }
            let positions = label_positions(node);
            for var in new_local_intervals {
                let start_chain = first_label_in_chain(&node.nodes, &positions, var.start);
                let merged = self.locals.current.iter().copied().find(|&id| {
                    let known = &self.locals.intervals[id];
                    known.name == var.name
                        && first_label_in_chain(&node.nodes, &positions, known.end) == start_chain
                });
                match merged {
                    // kotlinc moves the end without re-indexing it.
                    Some(id) => self.locals.intervals[id].end = var.end,
                    None => {
                        self.locals.add(var);
                    }
                }
            }
        }
        Ok(cursor)
    }

    /// `doClustering`: the blocks over the same range (by the first label of each end), in order.
    fn clusters(&self, node: &MethodNode, ids: &[usize]) -> Vec<Vec<usize>> {
        let positions = label_positions(node);
        let mut keys: Vec<(LabelId, LabelId)> = Vec::new();
        let mut clusters: Vec<Vec<usize>> = Vec::new();
        for &id in ids {
            let block = &self.tries.intervals[id];
            let key = (
                first_label_in_chain(&node.nodes, &positions, block.start),
                first_label_in_chain(&node.nodes, &positions, block.end),
            );
            match keys.iter().position(|&known| known == key) {
                Some(at) => clusters[at].push(id),
                None => {
                    keys.push(key);
                    clusters.push(vec![id]);
                }
            }
        }
        clusters
    }

    /// `findFinallyBlockBody`: the code between the end of block `id` and the start of the next
    /// block with the same default handler, cut at the `finallyEnd` of the depth its
    /// `finallyStart` names.
    fn finally_body(
        &self,
        node: &MethodNode,
        id: usize,
    ) -> Result<Option<FinallyBody>, InlineError> {
        let positions = label_positions(node);
        let chain = |label| first_label_in_chain(&node.nodes, &positions, label);
        let block = &self.tries.intervals[id];
        let mut same_default_handler = Vec::new();
        let mut default_handler = None;
        let mut after_block = false;
        for &other in &self.tries.order {
            if other == id {
                after_block = true;
            }
            if !after_block {
                continue;
            }
            let candidate = &self.tries.intervals[other];
            if candidate.catch_type.is_none()
                && ((chain(block.start) == chain(candidate.start)
                    && chain(block.end) == chain(candidate.end))
                    || default_handler == Some(chain(candidate.handler)))
            {
                same_default_handler.push(other);
                if default_handler.is_none() {
                    default_handler = Some(chain(candidate.handler));
                }
            }
        }
        if same_default_handler.is_empty() {
            return Ok(None);
        }
        let Some(&next) = same_default_handler.get(1) else {
            return Err(malformed("a finally has no second range"));
        };
        let at = |label: LabelId| {
            positions[label.index()].ok_or_else(|| malformed("a range ends at an unplaced label"))
        };
        let label_before = block.end;
        let start_chain = at(label_before)?;
        let meaningful = (start_chain + 1..node.nodes.len())
            .find(|&index| matches!(node.nodes[index], Node::Insn(_)))
            .ok_or_else(|| malformed("a finally has no instruction"))?;
        let depth =
            int_constant(&node.nodes[meaningful]).ok_or(InlineError::MalformedFinallyMarker)?;
        let mut end_exclusive = at(self.tries.intervals[next].start)?;
        let mut current = meaningful + 1;
        while end_exclusive != current {
            current += 1;
            if current >= node.nodes.len() {
                return Err(malformed("a finally runs past the body"));
            }
            if is_finally_end(&node.nodes[current]) {
                let current_depth = int_constant(&node.nodes[current - 1])
                    .ok_or(InlineError::MalformedFinallyMarker)?;
                if current_depth == depth {
                    end_exclusive = current + 1;
                    break;
                }
            }
        }
        let label_after = node.nodes[end_exclusive..]
            .iter()
            .find_map(|entry| match entry {
                Node::Label(label) => Some(*label),
                _ => None,
            })
            .ok_or_else(|| malformed("no label follows a finally"))?;
        let start = start_chain + 1;
        if start >= end_exclusive {
            return Err(malformed("a finally ends before it starts"));
        }
        Ok(Some(FinallyBody {
            start,
            end_exclusive,
            label_before,
            label_after,
        }))
    }

    /// `findTryCatchBlocksInlinedInFinally`: each block starting or ending inside the body.
    fn tries_in_finally(&self, nodes: &[Node], body: &FinallyBody) -> Vec<(usize, Position)> {
        let mut found: Vec<(usize, Position)> = Vec::new();
        for entry in &nodes[body.start..body.end_exclusive] {
            let Node::Label(label) = entry else {
                continue;
            };
            for &id in self.tries.starting_at(*label) {
                found.push((id, Position::Start));
            }
            for &id in self.tries.ending_at(*label) {
                match found.iter_mut().find(|(known, _)| *known == id) {
                    Some((_, position)) => *position = Position::Inner,
                    None => found.push((id, Position::End)),
                }
            }
        }
        found
    }

    /// `updateExceptionTable`: give the copied `finally` copies of the blocks wholly inside it,
    /// rejoin the ranges a nested `finally` split, and cut the copy out of the blocks around the
    /// return.
    fn update_exception_table(
        &mut self,
        node: &MethodNode,
        updating: &[usize],
        (new_finally_start, new_finally_end): (LabelId, LabelId),
        in_finally: &[(usize, Position)],
        old_to_new: &HashMap<LabelId, LabelId>,
        inserted_block_end: LabelId,
    ) -> Result<(), InlineError> {
        let new_label = |label: LabelId| {
            old_to_new
                .get(&label)
                .copied()
                .ok_or_else(|| malformed("a block in a finally ends outside it"))
        };
        let positions = label_positions(node);
        let mut keys: Vec<(LabelId, LabelId)> = Vec::new();
        let mut clusters: Vec<Vec<(usize, Position)>> = Vec::new();
        for &(id, position) in in_finally {
            let block = &self.tries.intervals[id];
            let key = (
                first_label_in_chain(&node.nodes, &positions, block.start),
                first_label_in_chain(&node.nodes, &positions, block.end),
            );
            match keys.iter().position(|&known| known == key) {
                Some(at) => clusters[at].push((id, position)),
                None => {
                    keys.push(key);
                    clusters.push(vec![(id, position)]);
                }
            }
        }
        let default_handler = |cluster: &[(usize, Position)], tries: &Intervals<TryBlock>| {
            cluster
                .iter()
                .find(|(id, _)| tries.intervals[*id].catch_type.is_none())
                .map(|(id, _)| tries.intervals[*id].handler)
                .ok_or_else(|| malformed("a split finally range has no default handler"))
        };
        let mut by_handler: Vec<(LabelId, Vec<(usize, Position)>)> = Vec::new();
        for cluster in clusters {
            match cluster[0].1 {
                Position::Inner => {
                    for &(id, _) in &cluster {
                        let block = self.tries.intervals[id].clone();
                        self.tries.add(TryBlock {
                            start: new_label(block.start)?,
                            end: new_label(block.end)?,
                            handler: old_to_new
                                .get(&block.handler)
                                .copied()
                                .unwrap_or(block.handler),
                            catch_type: block.catch_type,
                            only_copy: true,
                        });
                    }
                }
                Position::End => {
                    let handler = default_handler(&cluster, &self.tries)?;
                    by_handler.push((handler, cluster));
                }
                Position::Start => {
                    let handler = default_handler(&cluster, &self.tries)?;
                    let at = by_handler
                        .iter()
                        .position(|(known, _)| *known == handler)
                        .ok_or_else(|| malformed("a split finally range has no end"))?;
                    let (_, end_cluster) = by_handler.remove(at);
                    for (&(start_id, _), &(end_id, _)) in cluster.iter().zip(&end_cluster) {
                        let end_block = &self.tries.intervals[end_id];
                        let start_block = &self.tries.intervals[start_id];
                        if end_block.catch_type != start_block.catch_type {
                            return Err(malformed("a split finally range changes its catch"));
                        }
                        let by = (new_label(end_block.end)?, new_label(start_block.start)?);
                        self.tries.split(end_id, by.0, by.1, false);
                    }
                }
            }
        }
        if let [(_, single)] = by_handler.as_slice() {
            if single[0].1 == Position::End {
                for &(end_id, _) in single {
                    let by_start = new_label(self.tries.intervals[end_id].end)?;
                    self.tries
                        .split(end_id, by_start, inserted_block_end, false);
                }
                by_handler.clear();
            }
        }
        if !by_handler.is_empty() {
            return Err(malformed("unmatched split finally ranges"));
        }
        for &id in updating {
            self.tries
                .split_and_leave(id, new_finally_start, new_finally_end, false)?;
        }
        self.sort_tries(node, None);
        Ok(())
    }
}

fn remap_label(var: &mut LocalVariable, old: LabelId, new: LabelId) {
    if var.start == old {
        var.start = new;
    }
    if var.end == old {
        var.end = new;
    }
}

/// `insn` copied into a `finally` copy: a jump to a label inside the body goes to that label's
/// copy, a jump out of it keeps its target.
fn relabel(insn: &Insn, inside: &HashSet<LabelId>, map: &impl Fn(LabelId) -> LabelId) -> Insn {
    let target = |label: LabelId| {
        if inside.contains(&label) {
            map(label)
        } else {
            label
        }
    };
    match insn {
        Insn::Jump { op, target: to } => Insn::Jump {
            op: *op,
            target: target(*to),
        },
        Insn::TableSwitch {
            low,
            high,
            default,
            labels,
        } => Insn::TableSwitch {
            low: *low,
            high: *high,
            default: target(*default),
            labels: labels.iter().map(|&label| target(label)).collect(),
        },
        Insn::LookupSwitch {
            default,
            keys,
            labels,
        } => Insn::LookupSwitch {
            default: target(*default),
            keys: keys.clone(),
            labels: labels.iter().map(|&label| target(label)).collect(),
        },
        other => other.clone(),
    }
}
