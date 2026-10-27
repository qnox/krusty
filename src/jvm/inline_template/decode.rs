//! The walk from a [`MethodNode`] to a [`DecodedBody`].
//!
//! kotlinc's own analyzer (`FastMethodAnalyzer` with `OptimizationBasicInterpreter`) first types
//! every reached frame. Each reached block is then translated once, in class-file order: its entry
//! stack becomes block parameters typed by the analyzed frame, and its instructions become
//! operations over a stack of values. Control edges pass the values left on the stack to the
//! target block's parameters.

use crate::jvm::bytecode_passes::analysis::{analyze, BasicInterpreter, BasicValue, Frames};
use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::method_node::{Category, LabelId, MethodNode, Node};

use super::layout::Layout;
use super::model::{
    Anchor, Block, BlockId, DecodedBody, Edge, Handler, LocalId, LocalScope, Terminator, ValueDef,
    ValueId,
};
use super::translate::{jvm_type, BlockTranslator, BodyState, Exit};
use super::DecodeError;

const ACC_STATIC: u16 = 0x0008;

/// Decode `method`, a body of the class `owner` (an internal name), into blocks over typed values.
///
/// Only code some path reaches from the entry, or from a handler whose protected range is reached,
/// is decoded. A body the analyzer cannot type, a subroutine (`jsr`/`ret`), a stack manipulation
/// that splits a two-word value, or a block reached with stacks of different heights is rejected.
pub fn decode(owner: &str, method: &MethodNode) -> Result<DecodedBody, DecodeError> {
    let layout = Layout::of(method)?;
    let frames =
        analyze(method, owner, &mut BasicInterpreter).map_err(|error| DecodeError::Analysis {
            node: error.index,
            message: error.message,
        })?;
    let count = method.nodes.len();

    let starts: Vec<usize> = layout.block_starts().collect();
    let mut block_of = vec![None; count];
    let mut reached = Vec::new();
    for (position, &start) in starts.iter().enumerate() {
        if frames.is_reached(start) {
            let end = starts.get(position + 1).copied().unwrap_or(count);
            block_of[start] = Some(BlockId(reached.len() as u32));
            reached.push((start, end));
        }
    }
    if reached.is_empty() {
        return Err(DecodeError::malformed(
            0,
            "the method has no reachable code",
        ));
    }

    let mut state = BodyState::default();
    let parameters = parameter_locals(owner, method, &mut state)?;
    let targets = Targets {
        layout: &layout,
        block_of: &block_of,
    };
    let walk = Walk {
        method,
        frames: &frames,
        targets: &targets,
    };
    let mut positions = vec![None; count];
    let mut blocks = Vec::with_capacity(reached.len());
    for (index, &range) in reached.iter().enumerate() {
        let block = BlockId(index as u32);
        blocks.push(walk.block(&mut state, block, range, &mut positions)?);
    }
    check_edges(&blocks)?;

    let mut handlers = Vec::new();
    for entry in &method.try_catch_blocks {
        let Some(handler) = targets.block_at_label(entry.handler)? else {
            continue;
        };
        let start = layout.label_instruction(entry.start)?.unwrap_or(count);
        let end = layout.label_instruction(entry.end)?.unwrap_or(count);
        let covered: Vec<BlockId> = reached
            .iter()
            .enumerate()
            .filter(|(_, &(first, _))| (start..end).contains(&first))
            .map(|(index, _)| BlockId(index as u32))
            .collect();
        if !covered.is_empty() {
            handlers.push(Handler {
                covered,
                handler,
                catch_type: entry.catch_type.clone(),
            });
        }
    }

    let anchors = anchors_after(&positions);
    let mut local_scopes = Vec::with_capacity(method.local_variables.len());
    for variable in &method.local_variables {
        let local = state.local(variable.slot, Category::of_descriptor(&variable.desc));
        local_scopes.push(LocalScope {
            local,
            name: variable.name.clone(),
            descriptor: variable.desc.clone(),
            start: anchors[layout.label_node(variable.start)?],
            end: anchors[layout.label_node(variable.end)?],
        });
    }

    Ok(DecodedBody {
        owner: owner.to_string(),
        name: method.name.clone(),
        descriptor: method.desc.clone(),
        access: method.access,
        parameters,
        locals: state.locals,
        values: state.values,
        blocks,
        handlers,
        local_scopes,
    })
}

/// The locals the descriptor's parameters arrive in, the receiver first for an instance method.
fn parameter_locals(
    owner: &str,
    method: &MethodNode,
    state: &mut BodyState,
) -> Result<Vec<LocalId>, DecodeError> {
    let arguments = descriptors::argument_types(&method.desc)
        .ok_or_else(|| DecodeError::malformed(0, "malformed method descriptor"))?;
    let receiver = (method.access & ACC_STATIC == 0).then(|| descriptors::of_internal_name(owner));
    let mut slot = 0u16;
    let mut locals = Vec::with_capacity(arguments.len() + 1);
    for descriptor in receiver.as_deref().into_iter().chain(arguments) {
        let category = Category::of_descriptor(descriptor);
        locals.push(state.local(slot, category));
        slot += category.words() as u16;
    }
    Ok(locals)
}

/// Where labels lead among the reached blocks.
struct Targets<'a> {
    layout: &'a Layout,
    block_of: &'a [Option<BlockId>],
}

impl Targets<'_> {
    /// The reached block `label` starts, `None` when no path reaches it.
    fn block_at_label(&self, label: LabelId) -> Result<Option<BlockId>, DecodeError> {
        let Some(instruction) = self.layout.label_instruction(label)? else {
            return Err(DecodeError::malformed(
                0,
                "a jump or handler past the end of the code",
            ));
        };
        Ok(self.block_of[instruction])
    }

    fn edge(&self, label: LabelId, stack: &[ValueId]) -> Result<Edge, DecodeError> {
        let target = self
            .block_at_label(label)?
            .ok_or_else(|| DecodeError::malformed(0, "a reached jump to an unreached label"))?;
        Ok(Edge {
            target,
            arguments: stack.to_vec(),
        })
    }

    /// The edge into the block that starts at instruction node `start`.
    fn fall_through(&self, start: usize, stack: &[ValueId]) -> Result<Edge, DecodeError> {
        let target = self.block_of.get(start).copied().flatten().ok_or_else(|| {
            DecodeError::malformed(start, "control falls off the end of the code")
        })?;
        Ok(Edge {
            target,
            arguments: stack.to_vec(),
        })
    }
}

/// What every block's translation reads: the body, its analyzed frames and its block targets.
struct Walk<'a> {
    method: &'a MethodNode,
    frames: &'a Frames<BasicValue>,
    targets: &'a Targets<'a>,
}

impl Walk<'_> {
    /// Translate the reached block `block`, the instruction nodes `start..end`, recording where each
    /// of its instructions lands in `positions`.
    fn block(
        &self,
        state: &mut BodyState,
        block: BlockId,
        (start, end): (usize, usize),
        positions: &mut [Option<Anchor>],
    ) -> Result<Block, DecodeError> {
        let Walk {
            method,
            frames,
            targets,
        } = *self;
        let entry = frames
            .get(start)
            .ok_or_else(|| DecodeError::malformed(start, "a reached block has no frame"))?;
        let mut parameters = Vec::with_capacity(entry.stack.len());
        for (index, value) in entry.stack.iter().enumerate() {
            let ty = jvm_type(value).ok_or_else(|| {
                DecodeError::malformed(start, "an untyped value on a block's entry stack")
            })?;
            let def = ValueDef::BlockParameter {
                block,
                index: index as u32,
            };
            parameters.push(state.new_value(ty, def));
        }
        let mut translator = BlockTranslator {
            state,
            block,
            stack: parameters.clone(),
            ops: Vec::new(),
            node: start,
            line: None,
        };
        let layout = targets.layout;
        for (index, node) in method.nodes.iter().enumerate().take(end).skip(start) {
            let Node::Insn(insn) = node else { continue };
            translator.node = index;
            translator.line = layout.line(index);
            positions[index] = Some(Anchor::At {
                block,
                op: translator.ops.len() as u32,
            });
            let locals = frames
                .get(index)
                .ok_or_else(|| DecodeError::malformed(index, "a reached instruction has no frame"))?
                .locals;
            let Some(exit) = translator.translate(insn, locals)? else {
                continue;
            };
            let stack = &translator.stack;
            let terminator = match exit {
                Exit::Goto(label) => Terminator::Goto(targets.edge(label, stack)?),
                Exit::Branch { condition, taken } => Terminator::Branch {
                    condition,
                    taken: targets.edge(taken, stack)?,
                    not_taken: targets.fall_through(end, stack)?,
                },
                Exit::Switch {
                    value,
                    cases,
                    default,
                } => Terminator::Switch {
                    value,
                    cases: cases
                        .into_iter()
                        .map(|(key, label)| Ok((key, targets.edge(label, stack)?)))
                        .collect::<Result<_, DecodeError>>()?,
                    default: targets.edge(default, stack)?,
                },
                Exit::Return(value) => Terminator::Return(value),
                Exit::Throw(value) => Terminator::Throw(value),
            };
            return Ok(Block {
                parameters,
                ops: translator.ops,
                terminator,
                terminator_line: layout.line(index),
            });
        }
        // The block runs into the next one, on the line of its last instruction.
        let terminator = Terminator::Goto(targets.fall_through(end, &translator.stack)?);
        let terminator_line = translator.line;
        Ok(Block {
            parameters,
            ops: translator.ops,
            terminator,
            terminator_line,
        })
    }
}

/// Every edge passes exactly one value per parameter of its target.
fn check_edges(blocks: &[Block]) -> Result<(), DecodeError> {
    let fits =
        |edge: &Edge| edge.arguments.len() == blocks[edge.target.0 as usize].parameters.len();
    for block in blocks {
        let consistent = match &block.terminator {
            Terminator::Goto(edge) => fits(edge),
            Terminator::Branch {
                taken, not_taken, ..
            } => fits(taken) && fits(not_taken),
            Terminator::Switch { cases, default, .. } => {
                fits(default) && cases.iter().all(|(_, edge)| fits(edge))
            }
            Terminator::Return(_) | Terminator::Throw(_) => true,
        };
        if !consistent {
            return Err(DecodeError::malformed(
                0,
                "a block is reached with operand stacks of different heights",
            ));
        }
    }
    Ok(())
}

/// For each node, the position of the first reached instruction at or after it.
fn anchors_after(positions: &[Option<Anchor>]) -> Vec<Anchor> {
    let mut anchors = vec![Anchor::End; positions.len() + 1];
    for index in (0..positions.len()).rev() {
        anchors[index] = positions[index].unwrap_or(anchors[index + 1]);
    }
    anchors
}
