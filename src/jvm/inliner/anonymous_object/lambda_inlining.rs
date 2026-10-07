//! The methods of a copy that inline the lambdas its constructor was passed: kotlinc's
//! `AnonymousObjectTransformer.inlineMethodAndUpdateGlobalResult`, a `MethodInliner` over each
//! method of the original with the lambdas as its functional arguments.
//!
//! The original reads a lambda from the field its constructor kept it in (`aload 0; getfield
//! $block`) and calls `invoke` on it. The copy has no such field: the read goes, and each `invoke`
//! becomes the lambda's body, whose captured values are read from the copy's own fields
//! (`$x$inlined`) where the lambda read its captured parameters. The method keeps its own returns,
//! its parameter checks and its variable names; only the lambdas' locals are added above its own.

use std::collections::HashMap;

use crate::jvm::bytecode_passes::fix_stack::fix_stack;
use crate::jvm::bytecode_passes::insn_list::EditableMethod;
use crate::jvm::method_node::{Category, Insn, MethodNode, Node};
use crate::jvm::source_map::SourceMap;

use super::super::lambda_expansion::{self, Lambda, SourceLines};
use super::super::{functional_arguments, preparation, try_blocks};
use super::super::{Binding, InlineError, Parameter, Parameters};
use super::{CopiedLines, RegenerationError};

const ALOAD: u8 = 0x19;
const GETFIELD: u8 = 0xb4;

/// A lambda a copy inlines, from the field of the original that held it.
pub(super) struct FieldLambda<'a> {
    /// The original's field for the lambda.
    pub field: String,
    pub lambda: &'a Lambda,
    /// The copy's fields for the lambda's captured values, as `(name, descriptor)`.
    pub captured: Vec<(String, String)>,
}

/// Whether `method` reads the field of one of `lambdas`.
pub(super) fn reads_lambda(method: &MethodNode, lambdas: &[FieldLambda<'_>]) -> bool {
    method.instructions().any(|insn| {
        matches!(insn, Insn::Field { op: GETFIELD, name, .. }
            if lambdas.iter().any(|lambda| lambda.field == *name))
    })
}

/// `method` of the copy `owner`, its types already renamed, with every `invoke` of one of `lambdas`
/// replaced by the lambda's body. Its own lines are mapped through `lines`; the lambdas' lines are
/// lines of the caller, resolved through `caller` and mapped into the copy's map.
pub(super) fn inline_into(
    method: &MethodNode,
    owner: &str,
    lambdas: &[FieldLambda<'_>],
    lines: &mut CopiedLines,
    caller: &SourceMap,
) -> Result<MethodNode, RegenerationError> {
    let inlining = |error: InlineError| RegenerationError::Inlining(Box::new(error));
    let (parameters, placed) = method_parameters(method, owner, lambdas)?;
    let mut node = method.clone();
    preparation::add_captured_parameters(&mut node, &parameters);
    replace_lambda_reads(&mut node, owner, lambdas, parameters.real_size())?;
    try_blocks::move_try_starts_to_their_first_instruction(&mut node).map_err(inlining)?;
    let mut node = super::super::preprocess_node_before_inline(node).map_err(inlining)?;
    preparation::remove_fake_variable_initializations(&mut node);
    let invokes = functional_arguments::mark_places(&mut node, &parameters).map_err(inlining)?;
    let context = lambda_expansion::Context {
        parameters: &parameters,
        lambdas: &placed,
        inline_only: false,
        inline_markers: true,
    };
    let mut method_lines = MethodLines {
        lines,
        caller,
        own: HashMap::new(),
        lambda: HashMap::new(),
        failed: None,
    };
    let node =
        lambda_expansion::expand(&node, &context, invokes, &mut method_lines).map_err(inlining)?;
    if let Some(error) = method_lines.failed {
        return Err(error);
    }
    let mut node = parameters.remap(&node, 0).map_err(inlining)?;
    // The captured values were parameters only while the lambdas were placed.
    node.desc = method.desc.clone();
    node.max_locals = max_locals(&node, parameters.real_size());
    // The class writer's FixStack: the stack each lambda was inlined over is saved around it, in
    // locals above every one the method uses.
    let mut editable = EditableMethod::new(node);
    fix_stack(&mut editable, owner).map_err(RegenerationError::FixStack)?;
    Ok(editable.finish())
}

/// `MaxLocalsCalculator`: the slots the parameters and every local instruction take.
fn max_locals(node: &MethodNode, parameters: u16) -> u16 {
    node.instructions()
        .filter_map(|insn| match insn {
            Insn::Var { op, slot } => {
                let words = if matches!(op, 0x16 | 0x18 | 0x37 | 0x39) {
                    2
                } else {
                    1
                };
                Some(slot + words)
            }
            Insn::Iinc { slot, .. } => Some(slot + 1),
            _ => None,
        })
        .fold(parameters, u16::max)
}

/// The method's own parameters (`this` first), which stay where they are, then one slot per lambda
/// and the lambdas' captured values, which live in the copy's fields; and the lambdas with their
/// captured values placed there.
fn method_parameters(
    method: &MethodNode,
    owner: &str,
    lambdas: &[FieldLambda<'_>],
) -> Result<(Parameters, Vec<Lambda>), RegenerationError> {
    let malformed = RegenerationError::Unsupported("a malformed method descriptor");
    let arguments =
        crate::jvm::bytecode_passes::descriptors::argument_types(&method.desc).ok_or(malformed)?;
    let own = std::iter::once(Category::Reference)
        .chain(arguments.iter().map(|ty| Category::of_descriptor(ty)))
        .map(|category| Parameter {
            category,
            binding: Binding::Temporary,
        })
        .collect();
    let mut captured: Vec<Parameter> = (0..lambdas.len())
        .map(|lambda| Parameter {
            category: Category::Reference,
            binding: Binding::Lambda(lambda),
        })
        .collect();
    let mut placed = Vec::with_capacity(lambdas.len());
    for lambda in lambdas {
        let start = captured.len();
        for (name, desc) in &lambda.captured {
            captured.push(Parameter {
                category: Category::of_descriptor(desc),
                binding: Binding::Field {
                    owner: owner.to_string(),
                    name: name.clone(),
                    desc: desc.clone(),
                },
            });
        }
        placed.push(Lambda {
            captured: start..captured.len(),
            ..lambda.lambda.clone()
        });
    }
    Ok((
        Parameters {
            parameters: own,
            captured,
        },
        placed,
    ))
}

/// Turn each read of a lambda's field (`aload 0; getfield`) into a load of the slot the lambda
/// takes among the method's captured parameters, which the analysis follows to its `invoke`s.
fn replace_lambda_reads(
    node: &mut MethodNode,
    owner: &str,
    lambdas: &[FieldLambda<'_>],
    first_captured: u16,
) -> Result<(), RegenerationError> {
    let mut at = 0;
    while at < node.nodes.len() {
        let Node::Insn(Insn::Field {
            op: GETFIELD,
            owner: field_owner,
            name,
            ..
        }) = &node.nodes[at]
        else {
            at += 1;
            continue;
        };
        let Some(lambda) = lambdas.iter().position(|lambda| lambda.field == *name) else {
            at += 1;
            continue;
        };
        let from_this = at > 0
            && matches!(
                node.nodes[at - 1],
                Node::Insn(Insn::Var { op: ALOAD, slot: 0 })
            );
        if field_owner != owner || !from_this {
            return Err(RegenerationError::Unsupported(
                "a lambda's field read other than from this",
            ));
        }
        node.nodes[at - 1] = Node::Insn(Insn::Var {
            op: ALOAD,
            slot: first_captured + lambda as u16,
        });
        node.nodes.remove(at);
    }
    Ok(())
}

/// The lines of one method of the copy (one `SourceMapCopier` for its own lines, one for the
/// lambdas'): each line maps once and keeps that mapping.
struct MethodLines<'a> {
    lines: &'a mut CopiedLines,
    caller: &'a SourceMap,
    own: HashMap<u16, u16>,
    lambda: HashMap<u16, u16>,
    failed: Option<RegenerationError>,
}

impl SourceLines for MethodLines<'_> {
    fn map(&mut self, line: u16) -> Option<u16> {
        if let Some(&known) = self.own.get(&line) {
            return Some(known);
        }
        match self.lines.map(line) {
            Ok(to) => {
                self.own.insert(line, to);
                Some(to)
            }
            Err(error) => {
                self.failed.get_or_insert(error);
                None
            }
        }
    }

    fn synthetic(&mut self) -> Option<u16> {
        None
    }

    fn call_site(&self) -> Option<u16> {
        None
    }

    fn lambda(&mut self, line: u16) -> Option<u16> {
        if let Some(&known) = self.lambda.get(&line) {
            return Some(known);
        }
        let mapped = self
            .caller
            .resolve(line)
            .and_then(|(name, path, source, call_site)| {
                self.lines
                    .map
                    .map_copied_line(name, path, source, call_site)
            });
        match mapped {
            Some(to) => {
                self.lambda.insert(line, to);
                Some(to)
            }
            None => {
                self.failed.get_or_insert(RegenerationError::Unsupported(
                    "a lambda line the caller's source map does not cover",
                ));
                None
            }
        }
    }
}
