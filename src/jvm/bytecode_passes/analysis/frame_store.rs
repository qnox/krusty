//! The frames an analysis keeps, one before each reachable node, in one shared buffer.
//!
//! A method's analysis records a frame at every node it reaches. Kept as a separate [`Frame`] each,
//! they cost two allocations per node (the locals and the operand stack) and as many frees when the
//! result is dropped, on every pass that analyzes the method. Here every frame's values sit side by
//! side in one vector, and a node holds only where its own values start.
//!
//! An analysis may also leave a reached node's frame out (see [`super::analyze_kept`]): the node
//! is then known to be reached, and asking for its frame is a bug in the pass that chose not to
//! keep it.

use super::frame::{Frame, Interpreter, Value};

/// The frame before each node of a method, `None` where no path reaches.
#[derive(Debug)]
pub(crate) struct Frames<V> {
    /// Every recorded frame's locals followed by its operand stack, bottom first.
    values: Vec<V>,
    slots: Vec<Entry>,
}

/// What the analysis knows of one node.
#[derive(Clone, Copy, Debug)]
enum Entry {
    /// No path reaches the node.
    Unreached,
    /// A path reaches the node, and the analysis did not keep its frame.
    Reached,
    Recorded(Slot),
}

/// Where a node's frame sits in [`Frames::values`].
#[derive(Clone, Copy, Debug)]
struct Slot {
    start: usize,
    locals: usize,
    stack: usize,
    /// The operand-stack values the region has room for; a deeper stack moves the frame.
    room: usize,
}

/// One recorded frame: its locals by slot and its operand stack, bottom first.
#[derive(Debug, PartialEq)]
pub(crate) struct FrameView<'a, V> {
    pub locals: &'a [V],
    pub stack: &'a [V],
}

// Two borrowed slices copy whatever the values are; a derive would require `V: Copy`.
impl<V> Clone for FrameView<'_, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for FrameView<'_, V> {}

impl<V: Value> Frames<V> {
    /// No frame yet before any of `count` nodes.
    pub(super) fn new(count: usize) -> Self {
        Frames {
            values: Vec::new(),
            slots: vec![Entry::Unreached; count],
        }
    }

    /// The number of nodes, reached or not.
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// The frame before node `index`; `None` when no path reaches it or it is past the end.
    /// Panics for a reached node whose frame the analysis was told not to keep.
    pub(crate) fn get(&self, index: usize) -> Option<FrameView<'_, V>> {
        let slot = match *self.slots.get(index)? {
            Entry::Unreached => return None,
            Entry::Reached => panic!("the analysis did not keep the frame before node {index}"),
            Entry::Recorded(slot) => slot,
        };
        let locals_end = slot.start + slot.locals;
        Some(FrameView {
            locals: &self.values[slot.start..locals_end],
            stack: &self.values[locals_end..locals_end + slot.stack],
        })
    }

    /// The frame before each node, in node order.
    pub(crate) fn iter(&self) -> Iter<'_, V> {
        Iter {
            frames: self,
            nodes: 0..self.slots.len(),
        }
    }

    /// Whether a frame was recorded before node `index`.
    pub(super) fn is_recorded(&self, index: usize) -> bool {
        matches!(self.slots[index], Entry::Recorded(_))
    }

    /// Whether some path reaches node `index`, whether or not its frame was kept.
    pub(crate) fn is_reached(&self, index: usize) -> bool {
        !matches!(self.slots[index], Entry::Unreached)
    }

    /// Note that a path reaches node `index` without keeping its frame. A frame recorded there
    /// earlier stays: the analysis replaces it rather than calling this.
    pub(super) fn mark_reached(&mut self, index: usize) {
        if let Entry::Unreached = self.slots[index] {
            self.slots[index] = Entry::Reached;
        }
    }

    /// Replace `frame`'s locals and stack with the ones recorded before node `index`, keeping its
    /// allocations and its return value.
    pub(super) fn copy_into(&self, index: usize, frame: &mut Frame<V>) {
        let recorded = self.get(index).expect("a queued node has a frame");
        frame.locals.clear();
        frame.locals.extend_from_slice(recorded.locals);
        frame.stack.clear();
        frame.stack.extend_from_slice(recorded.stack);
    }

    /// Record `state` as the frame before node `index`, which has none yet.
    pub(super) fn record(&mut self, index: usize, state: &Frame<V>) {
        let start = self.values.len();
        self.values.extend_from_slice(&state.locals);
        self.values.extend_from_slice(&state.stack);
        self.slots[index] = Entry::Recorded(Slot {
            start,
            locals: state.locals.len(),
            stack: state.stack.len(),
            room: state.stack.len(),
        });
    }

    /// Replace the frame before node `index` with `state` when they differ; `true` when they did.
    pub(super) fn replace(&mut self, index: usize, state: &Frame<V>) -> bool {
        let recorded = self.get(index).expect("the slot holds a frame");
        if recorded.locals == state.locals.as_slice() && recorded.stack == state.stack.as_slice() {
            return false;
        }
        let Entry::Recorded(slot) = self.slots[index] else {
            panic!("the slot holds a frame");
        };
        if slot.locals != state.locals.len() || slot.room < state.stack.len() {
            self.record(index, state);
            return true;
        }
        let region = &mut self.values[slot.start..slot.start + slot.locals + state.stack.len()];
        let (locals, stack) = region.split_at_mut(slot.locals);
        locals.clone_from_slice(&state.locals);
        stack.clone_from_slice(&state.stack);
        self.slots[index] = Entry::Recorded(Slot {
            stack: state.stack.len(),
            ..slot
        });
        true
    }

    /// Meet the frame before node `index` with an edge arriving in `state`; `true` when anything
    /// changed (ASM's `Frame.merge`).
    pub(super) fn merge<I: Interpreter<V = V>>(
        &mut self,
        index: usize,
        state: &Frame<V>,
        interpreter: &mut I,
    ) -> Result<bool, String> {
        let Entry::Recorded(slot) = self.slots[index] else {
            panic!("the slot holds a frame");
        };
        if slot.stack != state.stack.len() {
            return Err("incompatible stack heights".to_string());
        }
        let region = &mut self.values[slot.start..slot.start + slot.locals + slot.stack];
        let (locals, stack) = region.split_at_mut(slot.locals);
        let mut changed = false;
        for (mine, theirs) in locals.iter_mut().zip(&state.locals) {
            let met = interpreter.merge_local(mine, theirs);
            if met != *mine {
                *mine = met;
                changed = true;
            }
        }
        for (mine, theirs) in stack.iter_mut().zip(&state.stack) {
            let met = interpreter.merge_stack(mine, theirs);
            if met != *mine {
                *mine = met;
                changed = true;
            }
        }
        Ok(changed)
    }
}

/// Two analyses agree when they hold the same frame before every node, wherever each stored it.
impl<V: Value> PartialEq for Frames<V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && (0..self.len()).all(|index| match (self.slots[index], other.slots[index]) {
                (Entry::Recorded(_), Entry::Recorded(_)) => self.get(index) == other.get(index),
                (Entry::Unreached, Entry::Unreached) | (Entry::Reached, Entry::Reached) => true,
                _ => false,
            })
    }
}

/// The frame before each node of a [`Frames`], in node order.
pub(crate) struct Iter<'a, V> {
    frames: &'a Frames<V>,
    nodes: std::ops::Range<usize>,
}

impl<'a, V: Value> Iterator for Iter<'a, V> {
    type Item = Option<FrameView<'a, V>>;

    fn next(&mut self) -> Option<Self::Item> {
        self.nodes.next().map(|index| self.frames.get(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.nodes.size_hint()
    }
}

impl<V: Value> DoubleEndedIterator for Iter<'_, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.nodes.next_back().map(|index| self.frames.get(index))
    }
}

impl<V: Value> ExactSizeIterator for Iter<'_, V> {}

impl<'a, V: Value> IntoIterator for &'a Frames<V> {
    type Item = Option<FrameView<'a, V>>;
    type IntoIter = Iter<'a, V>;

    fn into_iter(self) -> Iter<'a, V> {
        self.iter()
    }
}
