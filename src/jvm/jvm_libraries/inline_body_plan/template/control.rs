//! Structured control flow from a decoded body's blocks.
//!
//! This is Ramsey's "Beyond Relooper" (ICFP 2022) translation for reducible graphs: a block that
//! is the target of a retreating edge becomes a [`Shape::Loop`] the edge continues; a block reached
//! by two or more forward edges is emitted after a [`Shape::Labeled`] region the edges break out
//! of, as the last of its immediate dominator's code; any other block is emitted in place at its
//! single forward edge. A retreating edge to a block that does not dominate its source makes the
//! graph irreducible, and a switch is not structured yet: both decline.

use crate::jvm::inline_template::{BlockId, DecodedBody, Terminator};
use crate::libraries::TemplateDecline;

/// A structured region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Shape {
    /// The body runs once; a [`Transfer::Continue`] to `header` re-enters it.
    Loop { header: BlockId, body: Vec<Shape> },
    /// The body runs once; a [`Transfer::Break`] to `follow` leaves it. The code of `follow` comes
    /// right after the region.
    Labeled { follow: BlockId, body: Vec<Shape> },
    /// The operations of `block`, then its terminator.
    Node { block: BlockId, exit: Exit },
}

/// How a [`Shape::Node`] ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Exit {
    Return,
    Throw,
    Goto(Transfer),
    Branch {
        taken: Transfer,
        not_taken: Transfer,
    },
}

/// One control edge, after the edge's block-parameter assignments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Transfer {
    Continue(BlockId),
    Break(BlockId),
    /// The target's code, emitted in place.
    Inline(Vec<Shape>),
}

/// The structured form of `body`'s blocks.
pub(super) fn structure(body: &DecodedBody) -> Result<Vec<Shape>, TemplateDecline> {
    let graph = Graph::of(body)?;
    Ok(graph.tree(0))
}

struct Graph {
    successors: Vec<Vec<usize>>,
    /// Reverse-postorder number of each block; `usize::MAX` for a block the entry does not reach.
    rpo: Vec<usize>,
    /// Immediate dominator of each reached block (the entry's own).
    idom: Vec<usize>,
    /// Whether two or more forward edges reach the block.
    merge: Vec<bool>,
    /// Whether a retreating edge reaches the block.
    loop_header: Vec<bool>,
    /// Whether the block ends in `throw` rather than `return` when it has no successor.
    throws: Vec<bool>,
}

fn successors(terminator: &Terminator) -> Result<Vec<usize>, TemplateDecline> {
    Ok(match terminator {
        Terminator::Goto(edge) => vec![edge.target.0 as usize],
        Terminator::Branch {
            taken, not_taken, ..
        } => vec![taken.target.0 as usize, not_taken.target.0 as usize],
        Terminator::Switch { .. } => {
            return Err(TemplateDecline::UnstructuredControlFlow("a switch"))
        }
        Terminator::Return(_) | Terminator::Throw(_) => Vec::new(),
    })
}

impl Graph {
    fn of(body: &DecodedBody) -> Result<Graph, TemplateDecline> {
        let count = body.blocks.len();
        let successors = body
            .blocks
            .iter()
            .map(|block| successors(&block.terminator))
            .collect::<Result<Vec<_>, _>>()?;

        // Postorder by an explicit depth-first walk from the entry.
        let mut postorder = Vec::with_capacity(count);
        let mut visited = vec![false; count];
        let mut stack = vec![(0usize, 0usize)];
        visited[0] = true;
        while let Some((block, next)) = stack.last_mut() {
            if let Some(&successor) = successors[*block].get(*next) {
                *next += 1;
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            } else {
                postorder.push(*block);
                stack.pop();
            }
        }
        let mut rpo = vec![usize::MAX; count];
        for (number, &block) in postorder.iter().rev().enumerate() {
            rpo[block] = number;
        }
        let order: Vec<usize> = postorder.iter().rev().copied().collect();

        let mut predecessors = vec![Vec::new(); count];
        for (block, targets) in successors.iter().enumerate() {
            if rpo[block] == usize::MAX {
                continue;
            }
            for &target in targets {
                predecessors[target].push(block);
            }
        }

        // Cooper, Harvey and Kennedy's iterative dominators over reverse postorder.
        let mut idom = vec![usize::MAX; count];
        idom[0] = 0;
        let intersect = |idom: &[usize], mut a: usize, mut b: usize| {
            while a != b {
                while rpo[a] > rpo[b] {
                    a = idom[a];
                }
                while rpo[b] > rpo[a] {
                    b = idom[b];
                }
            }
            a
        };
        let mut changed = true;
        while changed {
            changed = false;
            for &block in order.iter().skip(1) {
                let mut processed = predecessors[block]
                    .iter()
                    .copied()
                    .filter(|&predecessor| idom[predecessor] != usize::MAX);
                let Some(first) = processed.next() else {
                    continue;
                };
                let new = processed.fold(first, |dominator, predecessor| {
                    intersect(&idom, predecessor, dominator)
                });
                if idom[block] != new {
                    idom[block] = new;
                    changed = true;
                }
            }
        }

        let dominates = |dominator: usize, mut block: usize| loop {
            if block == dominator {
                return true;
            }
            if block == 0 {
                return false;
            }
            block = idom[block];
        };
        let mut merge = vec![false; count];
        let mut loop_header = vec![false; count];
        for &block in &order {
            let forward = predecessors[block]
                .iter()
                .filter(|&&predecessor| rpo[predecessor] < rpo[block])
                .count();
            merge[block] = forward >= 2;
            for &predecessor in &predecessors[block] {
                if rpo[predecessor] >= rpo[block] {
                    if !dominates(block, predecessor) {
                        return Err(TemplateDecline::UnstructuredControlFlow(
                            "irreducible control flow",
                        ));
                    }
                    loop_header[block] = true;
                }
            }
        }
        let throws = body
            .blocks
            .iter()
            .map(|block| matches!(block.terminator, Terminator::Throw(_)))
            .collect();
        Ok(Graph {
            successors,
            throws,
            rpo,
            idom,
            merge,
            loop_header,
        })
    }

    /// The code of `block` and of the blocks it immediately dominates.
    fn tree(&self, block: usize) -> Vec<Shape> {
        // The merge nodes `block` immediately dominates, the latest in reverse postorder first: the
        // outermost labeled region is the one whose follow comes last.
        let mut merges: Vec<usize> = (0..self.idom.len())
            .filter(|&child| {
                child != block
                    && self.rpo[child] != usize::MAX
                    && self.idom[child] == block
                    && self.merge[child]
            })
            .collect();
        merges.sort_by_key(|&child| std::cmp::Reverse(self.rpo[child]));
        let code = self.within(block, &merges);
        if self.loop_header[block] {
            vec![Shape::Loop {
                header: BlockId(block as u32),
                body: code,
            }]
        } else {
            code
        }
    }

    fn within(&self, block: usize, merges: &[usize]) -> Vec<Shape> {
        let Some((&follow, inner)) = merges.split_first() else {
            return vec![Shape::Node {
                block: BlockId(block as u32),
                exit: self.exit(block),
            }];
        };
        let mut code = vec![Shape::Labeled {
            follow: BlockId(follow as u32),
            body: self.within(block, inner),
        }];
        code.extend(self.tree(follow));
        code
    }

    fn exit(&self, block: usize) -> Exit {
        match self.successors[block].as_slice() {
            [] if self.throws[block] => Exit::Throw,
            [] => Exit::Return,
            [target] => Exit::Goto(self.transfer(block, *target)),
            [taken, not_taken] => Exit::Branch {
                taken: self.transfer(block, *taken),
                not_taken: self.transfer(block, *not_taken),
            },
            _ => unreachable!("a structured block has at most two successors"),
        }
    }

    fn transfer(&self, from: usize, to: usize) -> Transfer {
        if self.rpo[to] <= self.rpo[from] {
            Transfer::Continue(BlockId(to as u32))
        } else if self.merge[to] {
            Transfer::Break(BlockId(to as u32))
        } else {
            Transfer::Inline(self.tree(to))
        }
    }
}
