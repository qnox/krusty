//! The methods of a copy that inline the lambdas its constructor was passed: kotlinc's
//! `AnonymousObjectTransformer.inlineMethodAndUpdateGlobalResult`, a `MethodInliner` over each
//! method of the original with the lambdas as its functional arguments.
//!
//! The original reads a lambda from the field its constructor kept it in (`aload 0; getfield
//! $block`) and calls `invoke` on it. The copy has no such field: the read goes, and each `invoke`
//! becomes the lambda's body, whose captured values are read from the copy's own fields
//! (`$x$inlined`) where the lambda read its captured parameters. The method keeps its own returns,
//! its parameter checks and its variable names; only the lambdas' locals are added above its own.

use crate::fx_hash::HashMap;

use crate::jvm::bytecode_passes::fix_stack::fix_stack;
use crate::jvm::bytecode_passes::insn_list::EditableMethod;
use crate::jvm::method_node::{Category, Insn, MethodNode, Node};
use crate::jvm::source_map::SourceMap;

use super::super::class_roles::RegeneratedClass;
use super::super::lambda_expansion::{self, Lambda, SourceLines};
use super::super::{functional_arguments, object_regeneration, preparation, try_blocks};
use super::super::{Binding, ClassRoles, InlineError, Parameter, Parameters};
use super::{CopiedLines, NestedObjects, RegenerationError};
use crate::ir::InlineLambdaPlacement;

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

/// Whether `method` of `owner` invokes a lambda one of `fields` holds (`aload 0; getfield` of the
/// field, then `FunctionN.invoke` on it), as `functional_arguments::lambda_placements` places each
/// lambda the method reads: a read only passed on to the constructor of a regenerated object, as a
/// suspend lambda's `invoke` passes its lambdas to its own constructor, invokes nothing here. An
/// error names a use the inliner does not place.
pub(super) fn invokes_field_lambda(
    method: &MethodNode,
    owner: &str,
    fields: &[&str],
    classes: &dyn ClassRoles,
) -> Result<bool, RegenerationError> {
    let reads = method.instructions().any(|insn| {
        matches!(insn, Insn::Field { op: GETFIELD, name, .. } if fields.contains(&name.as_str()))
    });
    if !reads {
        return Ok(false);
    }
    let parameters = own_parameters(method, fields.len())?;
    let mut node = method.clone();
    preparation::add_captured_parameters(&mut node, &parameters);
    replace_lambda_reads(&mut node, owner, fields, parameters.real_size())?;
    let placements =
        functional_arguments::lambda_placements(&node, &parameters, fields.len(), classes)
            .map_err(|_error| {
                crate::trace_compiler!("splice", "a field lambda is not placed: {_error}");
                RegenerationError::Unsupported("a lambda use the inliner does not place")
            })?;
    Ok(placements.contains(&InlineLambdaPlacement::Invoked))
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
    objects: &mut NestedObjects<'_, '_>,
) -> Result<MethodNode, RegenerationError> {
    let inlining = |error: InlineError| RegenerationError::Inlining(Box::new(error));
    let (parameters, placed) = method_parameters(method, owner, lambdas)?;
    let mut node = method.clone();
    preparation::add_captured_parameters(&mut node, &parameters);
    let fields: Vec<&str> = lambdas.iter().map(|lambda| lambda.field.as_str()).collect();
    replace_lambda_reads(&mut node, owner, &fields, parameters.real_size())?;
    try_blocks::move_try_starts_to_their_first_instruction(&mut node).map_err(inlining)?;
    let mut node = super::super::preprocess_node_before_inline(node).map_err(inlining)?;
    preparation::remove_fake_variable_initializations(&mut node);
    let classes = WithOwnCopy {
        classes: objects.regeneration.classes,
        copy: owner,
    };
    let constructors =
        object_regeneration::regenerate_objects(&mut node, &parameters, &placed, &classes, objects)
            .map_err(inlining)?;
    let invokes = functional_arguments::mark_places(&mut node, &parameters).map_err(inlining)?;
    object_regeneration::complete_constructor_calls(&mut node, constructors).map_err(inlining)?;
    let context = lambda_expansion::Context {
        parameters: &parameters,
        lambdas: &placed,
        inline_only: false,
        inline_markers: true,
    };
    let mut method_lines = MethodLines {
        lines,
        caller,
        own: HashMap::default(),
        lambda: HashMap::default(),
        failed: None,
    };
    let lambda_expansion::Expanded {
        mut node,
        lambda_blocks,
    } = lambda_expansion::expand(&node, &context, invokes, &mut method_lines, None)
        .map_err(inlining)?;
    super::super::try_blocks::order_around_finally_blocks(&mut node, lambda_blocks);
    if let Some(error) = method_lines.failed {
        return Err(error);
    }
    let mut node = parameters.remap(&node, 0).map_err(inlining)?;
    // The captured values were parameters only while the lambdas were placed.
    node.desc = method.desc.clone();
    node.max_locals = node.locals_top().max(parameters.real_size());
    // The class writer's FixStack: the stack each lambda was inlined over is saved around it, in
    // locals above every one the method uses.
    let mut editable = EditableMethod::new(node);
    fix_stack(&mut editable, owner).map_err(RegenerationError::FixStack)?;
    Ok(editable.finish())
}

/// The method's own parameters (`this` first), which stay where they are, then one slot per lambda
/// and the lambdas' captured values, which live in the copy's fields; and the lambdas with their
/// captured values placed there.
fn method_parameters(
    method: &MethodNode,
    owner: &str,
    lambdas: &[FieldLambda<'_>],
) -> Result<(Parameters, Vec<Lambda>), RegenerationError> {
    let Parameters {
        parameters: own,
        mut captured,
        ..
    } = own_parameters(method, lambdas.len())?;
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
            capture_owners: Vec::new(),
            non_inline_suspend: Vec::new(),
        },
        placed,
    ))
}

/// The classes a method of the copy names, the copy among the regenerated ones: a method that
/// constructs the object anew (a suspend lambda's `invoke` or `create`) passes it the lambdas it
/// was passed, and that construction is the copy's own, already regenerated (kotlinc's
/// `isAlreadyRegenerated`), taking the lambdas' captured values in their place.
struct WithOwnCopy<'a> {
    classes: &'a dyn ClassRoles,
    copy: &'a str,
}

impl ClassRoles for WithOwnCopy<'_> {
    fn regenerated_class(&self, internal: &str) -> Option<RegeneratedClass> {
        if internal == self.copy {
            return Some(RegeneratedClass::AnonymousObject);
        }
        self.classes.regenerated_class(internal)
    }

    fn contains_reified_marker(&self, internal: &str) -> bool {
        internal != self.copy && self.classes.contains_reified_marker(internal)
    }

    fn class_file(&self, internal: &str) -> Option<Vec<u8>> {
        self.classes.class_file(internal)
    }
}

/// The method's own parameters (`this` first), then one slot for each of `lambdas` lambdas.
fn own_parameters(method: &MethodNode, lambdas: usize) -> Result<Parameters, RegenerationError> {
    let malformed = RegenerationError::Unsupported("a malformed method descriptor");
    let arguments =
        crate::jvm::bytecode_passes::descriptors::argument_types(&method.desc).ok_or(malformed)?;
    let parameters = std::iter::once(Category::Reference)
        .chain(arguments.iter().map(|ty| Category::of_descriptor(ty)))
        .map(|category| Parameter {
            category,
            binding: Binding::Temporary,
        })
        .collect();
    let captured = (0..lambdas)
        .map(|lambda| Parameter {
            category: Category::Reference,
            binding: Binding::Lambda(lambda),
        })
        .collect();
    Ok(Parameters {
        parameters,
        captured,
        capture_owners: Vec::new(),
        non_inline_suspend: Vec::new(),
    })
}

/// Turn each read of a lambda's field (`aload 0; getfield`) into a load of the slot the lambda
/// takes among the method's captured parameters, which the analysis follows to its `invoke`s.
fn replace_lambda_reads(
    node: &mut MethodNode,
    owner: &str,
    fields: &[&str],
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
        let Some(lambda) = fields.iter().position(|field| *field == name) else {
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
