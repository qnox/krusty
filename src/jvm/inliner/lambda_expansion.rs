//! The body placed into the caller with its inline lambdas expanded: kotlinc's
//! `MethodInliner.doInline(node)`, whose visitor runs the body through a `LocalVariablesSorter` and
//! replaces each `invoke` of a lambda with the lambda's own inlined body.
//!
//! At an `invoke` the arguments are on the stack as `Object`s. Each is coerced to the lambda's
//! parameter type and stored, the last first, to slots above every local the body has used so far
//! (and above its inline markers); the lambda's body is inlined with its parameters read from those
//! slots and its captured values from the caller's; its result is boxed back to the `Object`
//! `invoke` returns. The lambda's code goes through the same sorter as the body's, so its locals
//! share the numbering the body's locals get.

use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::method_node::{Category, Insn, LabelId, LocalVariable, MethodNode, Node};

use super::functional_arguments::is_invoke_on_lambda;
use super::local_sorter::Sorter;
use super::{preparation, returns, InlineError, Parameter, Parameters};

const NOP: u8 = 0x00;
const CHECKCAST: u8 = 0xc0;
const GETSTATIC: u8 = 0xb2;
const INVOKEVIRTUAL: u8 = 0xb6;
const INVOKESTATIC: u8 = 0xb8;

/// `$i$f$` and `$i$a$`: the marker variables of an inlined function and an inlined lambda
/// (`JvmAbi.isFakeLocalVariableForInline`).
const INLINE_MARKER_PREFIXES: [&str; 2] = ["$i$f$", "$i$a$"];

/// An inline lambda argument, as kotlinc's `LambdaInfo` hands it to the inliner.
#[derive(Clone, Debug)]
pub(crate) struct Lambda {
    /// The lambda's compiled body: its parameters (an extension receiver first) from slot 0, then
    /// the values it captures, then its own locals. It ends each path with a return of
    /// `return_type`.
    pub node: MethodNode,
    /// The descriptors of the lambda's parameters (`invokeMethod.argumentTypes`).
    pub parameter_types: Vec<String>,
    /// The descriptor of what the lambda returns (`invokeMethod.returnType`), `V` for `Unit`.
    pub return_type: String,
    /// The lambda's captured values: this range of [`Parameters::captured`].
    pub captured: std::ops::Range<usize>,
}

/// How the inlined code's line numbers reach the caller's line table (kotlinc's `SourceMapper`).
pub(crate) trait SourceLines {
    /// `SourceMapCopier.mapLineNumber` of a line of the inlined function's own file; `None` for a
    /// line nothing maps, which is dropped.
    fn map(&mut self, line: u16) -> Option<u16>;
    /// `mapSyntheticLineNumber(LOCAL_VARIABLE_INLINE_ARGUMENT_SYNTHETIC_LINE_NUMBER)`: the line
    /// that separates an `@InlineOnly` call's line from a lambda starting on the same line.
    fn synthetic(&mut self) -> Option<u16>;
    /// The line of the call.
    fn call_site(&self) -> Option<u16>;
}

/// A lambda's lines are lines of the caller's own file, which map to themselves.
struct CallerLines;

impl SourceLines for CallerLines {
    fn map(&mut self, line: u16) -> Option<u16> {
        Some(line)
    }
    fn synthetic(&mut self) -> Option<u16> {
        None
    }
    fn call_site(&self) -> Option<u16> {
        None
    }
}

/// What the body being placed is.
pub(super) struct Context<'a> {
    pub parameters: &'a Parameters,
    pub lambdas: &'a [Lambda],
    pub inline_only: bool,
}

/// kotlinc's `calcMarkerShift`: past the last inline marker variable, in the slots the prepared
/// body's parameters take.
fn marker_shift(parameters: &Parameters, node: &MethodNode) -> i32 {
    let after_last_marker = node
        .local_variables
        .iter()
        .filter(|local| {
            INLINE_MARKER_PREFIXES
                .iter()
                .any(|prefix| local.name.starts_with(prefix))
        })
        .map(|local| i32::from(local.slot) + 1)
        .max()
        .unwrap_or(-1);
    after_last_marker - i32::from(parameters.real_size()) + i32::from(parameters.args_size())
}

fn var_size(op: u8) -> u16 {
    if matches!(op, 0x16 | 0x18 | 0x37 | 0x39) {
        2
    } else {
        1
    }
}

fn wrapper(primitive: &str) -> Option<(&'static str, &'static str)> {
    Some(match primitive {
        "Z" => ("java/lang/Boolean", "boolean"),
        "C" => ("java/lang/Character", "char"),
        "B" => ("java/lang/Byte", "byte"),
        "S" => ("java/lang/Short", "short"),
        "I" => ("java/lang/Integer", "int"),
        "J" => ("java/lang/Long", "long"),
        "F" => ("java/lang/Float", "float"),
        "D" => ("java/lang/Double", "double"),
        _ => return None,
    })
}

fn method(op: u8, owner: &str, name: &str, desc: &str) -> Node {
    Node::Insn(Insn::Method {
        op,
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: false,
    })
}

/// `StackValue.coerce` from the `Object` `invoke` receives to a lambda parameter's type.
fn coerce_from_object(desc: &str) -> Vec<Node> {
    if let Some((owner, primitive)) = wrapper(desc) {
        let class = if matches!(desc, "Z" | "C") {
            owner
        } else {
            "java/lang/Number"
        };
        return vec![
            Node::Insn(Insn::Type {
                op: CHECKCAST,
                class: class.to_string(),
            }),
            method(
                INVOKEVIRTUAL,
                class,
                &format!("{primitive}Value"),
                &format!("(){desc}"),
            ),
        ];
    }
    if desc == "Ljava/lang/Object;" {
        return Vec::new();
    }
    vec![Node::Insn(Insn::Type {
        op: CHECKCAST,
        class: descriptors::internal_name(desc).to_string(),
    })]
}

/// `StackValue.coerce` from a lambda's result to the `Object` `invoke` returns.
fn coerce_to_object(desc: &str) -> Vec<Node> {
    if desc == "V" {
        return vec![Node::Insn(Insn::Field {
            op: GETSTATIC,
            owner: "kotlin/Unit".to_string(),
            name: "INSTANCE".to_string(),
            desc: "Lkotlin/Unit;".to_string(),
        })];
    }
    match wrapper(desc) {
        Some((owner, _)) => vec![method(
            INVOKESTATIC,
            owner,
            "valueOf",
            &format!("({desc})L{owner};"),
        )],
        None => Vec::new(),
    }
}

/// Copies nodes of another method into `out`, giving each of its labels a fresh label of `out`.
struct LabelImport {
    labels: Vec<LabelId>,
}

impl LabelImport {
    fn new(from: &MethodNode, out: &mut MethodNode) -> LabelImport {
        LabelImport {
            labels: (0..from.label_count()).map(|_| out.new_label()).collect(),
        }
    }

    fn label(&self, label: LabelId) -> LabelId {
        self.labels[label.index()]
    }

    fn insn(&self, insn: &Insn) -> Insn {
        match insn {
            Insn::Jump { op, target } => Insn::Jump {
                op: *op,
                target: self.label(*target),
            },
            Insn::TableSwitch {
                low,
                high,
                default,
                labels,
            } => Insn::TableSwitch {
                low: *low,
                high: *high,
                default: self.label(*default),
                labels: labels.iter().map(|label| self.label(*label)).collect(),
            },
            Insn::LookupSwitch {
                default,
                keys,
                labels,
            } => Insn::LookupSwitch {
                default: self.label(*default),
                keys: keys.clone(),
                labels: labels.iter().map(|label| self.label(*label)).collect(),
            },
            other => other.clone(),
        }
    }
}

/// The body with its lambdas expanded and every local renumbered, its lines mapped into the
/// caller's source map. `invokes` names the lambda each `FunctionN.invoke` of the body calls.
pub(super) fn expand(
    node: &MethodNode,
    context: &Context<'_>,
    invokes: Vec<Option<usize>>,
    lines: &mut dyn SourceLines,
) -> Result<MethodNode, InlineError> {
    let mut out = node.clone();
    out.nodes = Vec::with_capacity(node.nodes.len());
    out.local_variables = Vec::new();
    out.try_catch_blocks = Vec::new();
    let args_size = context.parameters.args_size();
    let mut sorter = Sorter::new(args_size);
    let mut next_local_index = i32::from(args_size);
    let marker_shift = marker_shift(context.parameters, node);
    let mut current_line: i32 = if context.inline_only {
        lines.call_site().map_or(-1, i32::from)
    } else {
        -1
    };
    let mut invokes = invokes.into_iter();
    for entry in &node.nodes {
        match entry {
            Node::Line { line, start } => {
                if !context.inline_only {
                    current_line = i32::from(*line);
                }
                if let Some(line) = lines.map(*line) {
                    out.nodes.push(Node::Line {
                        line,
                        start: *start,
                    });
                }
            }
            Node::Insn(insn) if is_invoke_on_lambda(insn) => {
                let lambda = invokes.next().ok_or_else(|| {
                    InlineError::Analysis("an invoke the analysis did not see".to_string())
                })?;
                match lambda {
                    None => out.nodes.push(entry.clone()),
                    Some(lambda) => {
                        let expansion = Expansion {
                            context,
                            lambda: &context.lambdas[lambda],
                            marker_shift,
                            current_line,
                        };
                        let Insn::Method { desc, .. } = insn else {
                            unreachable!("an invoke is a method instruction");
                        };
                        expansion.expand(desc, &mut out, &mut sorter, next_local_index, lines)?;
                    }
                }
            }
            Node::Insn(insn) => {
                if let Insn::Var { op, slot } = insn {
                    next_local_index =
                        next_local_index.max(i32::from(*slot) + i32::from(var_size(*op)));
                } else if let Insn::Iinc { slot, .. } = insn {
                    next_local_index = next_local_index.max(i32::from(*slot) + 1);
                }
                let mut insn = insn.clone();
                sorter.visit(&mut insn);
                out.nodes.push(Node::Insn(insn));
            }
            Node::Label(_) => out.nodes.push(entry.clone()),
        }
    }
    // The body's try/catch blocks and variable table are visited last.
    out.try_catch_blocks
        .extend(node.try_catch_blocks.iter().cloned());
    for local in &node.local_variables {
        let mut local = local.clone();
        sorter.visit_local(&mut local);
        out.local_variables.push(local);
    }
    out.max_locals = sorter.max_locals();
    Ok(out)
}

/// One `invoke` of a lambda, being replaced by the lambda's body.
struct Expansion<'a> {
    context: &'a Context<'a>,
    lambda: &'a Lambda,
    marker_shift: i32,
    current_line: i32,
}

impl Expansion<'_> {
    fn expand(
        &self,
        invoke_desc: &str,
        out: &mut MethodNode,
        sorter: &mut Sorter,
        next_local_index: i32,
        lines: &mut dyn SourceLines,
    ) -> Result<(), InlineError> {
        let types = &self.lambda.parameter_types;
        let arguments = descriptors::argument_types(invoke_desc)
            .ok_or_else(|| InlineError::Analysis(format!("malformed descriptor {invoke_desc}")))?;
        if arguments.len() != types.len() {
            return Err(InlineError::LambdaArity);
        }
        let mut value_param_shift = next_local_index.max(self.marker_shift)
            + types
                .iter()
                .map(|ty| descriptors::size(ty) as i32)
                .sum::<i32>();
        // kotlinc stores the arguments with `InstructionAdapter.store`, which writes to the
        // delegate and so never advances `InlineAdapter.nextLocalIndex`: a later invoke stores its
        // arguments in the same slots.
        for ty in types.iter().rev() {
            out.nodes.extend(coerce_from_object(ty));
            value_param_shift -= descriptors::size(ty) as i32;
            let slot = u16::try_from(value_param_shift).map_err(|_| InlineError::LambdaArity)?;
            let op = Category::of_descriptor(ty).store_op();
            let mut store = Insn::Var { op, slot };
            sorter.visit(&mut store);
            out.nodes.push(Node::Insn(store));
        }
        if types.is_empty() {
            out.nodes.push(Node::Insn(Insn::Op(NOP)));
        }
        let first_line = self.lambda.node.nodes.iter().find_map(|node| match node {
            Node::Line { line, .. } => Some(i32::from(*line)),
            _ => None,
        });
        // An `@InlineOnly` call's code all has the call's line, so a lambda starting on that line
        // would run into it unseen: a synthetic line keeps the two apart for the debugger.
        if self.context.inline_only
            && self.current_line >= 0
            && first_line == Some(self.current_line)
        {
            let label = out.new_label();
            out.nodes.push(Node::Label(label));
            if let Some(line) = lines.synthetic() {
                out.nodes.push(Node::Line { line, start: label });
            }
        }
        let slot = u16::try_from(value_param_shift).map_err(|_| InlineError::LambdaArity)?;
        let inlined = inline_lambda(self.lambda, self.context.parameters, slot)?;
        let import = LabelImport::new(&inlined, out);
        for block in &inlined.try_catch_blocks {
            out.try_catch_blocks
                .push(crate::jvm::method_node::TryCatchBlock {
                    start: import.label(block.start),
                    end: import.label(block.end),
                    handler: import.label(block.handler),
                    catch_type: block.catch_type.clone(),
                });
        }
        for node in &inlined.nodes {
            out.nodes.push(match node {
                Node::Label(label) => Node::Label(import.label(*label)),
                Node::Line { line, start } => Node::Line {
                    line: *line,
                    start: import.label(*start),
                },
                Node::Insn(insn) => {
                    let mut insn = import.insn(insn);
                    sorter.visit(&mut insn);
                    Node::Insn(insn)
                }
            });
        }
        for local in &inlined.local_variables {
            let mut local = LocalVariable {
                start: import.label(local.start),
                end: import.label(local.end),
                ..local.clone()
            };
            sorter.visit_local(&mut local);
            out.local_variables.push(local);
        }
        out.nodes.extend(coerce_to_object(&self.lambda.return_type));
        if self.current_line >= 0 {
            let line = u16::try_from(self.current_line).expect("a line number fits u16");
            let line = if self.context.inline_only {
                Some(line)
            } else {
                lines.map(line)
            };
            let label = out.new_label();
            out.nodes.push(Node::Label(label));
            if let Some(line) = line {
                out.nodes.push(Node::Line { line, start: label });
            }
        }
        Ok(())
    }
}

/// The lambda's own `MethodInliner.doInline` at an `invoke` whose arguments were stored from
/// `value_param_shift`: its parameters are read from there, its captured values from the caller's
/// (the body's captured parameters), and its locals follow its parameters. Every return becomes a
/// jump to the end, leaving its value on the stack.
fn inline_lambda(
    lambda: &Lambda,
    host: &Parameters,
    value_param_shift: u16,
) -> Result<MethodNode, InlineError> {
    let parameters = lambda_parameters(lambda, host);
    let mut node = lambda.node.clone();
    super::try_blocks::move_try_starts_to_their_first_instruction(&mut node)?;
    preparation::remove_fake_variable_initializations(&mut node);
    returns::normalize_local_returns(&mut node)?;
    let invokes = super::functional_arguments::mark_places(&mut node, &parameters)?;
    let context = Context {
        parameters: &parameters,
        lambdas: &[],
        inline_only: false,
    };
    let mut node = expand(&node, &context, invokes, &mut CallerLines)?;
    preparation::remove_closure_assertions(&mut node)?;
    let mut node = remap_lambda(&node, lambda, host, value_param_shift)?;
    let end = node.new_label();
    node.nodes.push(Node::Label(end));
    returns::process_returns(&mut node, end);
    Ok(node)
}

/// The lambda's parameters as its own inliner sees them: the real ones, then its captured values.
fn lambda_parameters(lambda: &Lambda, host: &Parameters) -> Parameters {
    let real = lambda
        .parameter_types
        .iter()
        .map(|ty| Parameter {
            category: Category::of_descriptor(ty),
            binding: super::Binding::Temporary,
        })
        .collect();
    Parameters {
        parameters: real,
        captured: host.captured[lambda.captured.clone()].to_vec(),
    }
}

/// `LocalVarRemapper(lambdaParameters, valueParamShift)`: a real parameter reads the slot its
/// argument was stored to, a captured value reads the body's captured parameter (so the body's own
/// binding of it decides where it lives), and a local moves above the stored arguments. Only
/// what moved keeps its variable-table entry.
fn remap_lambda(
    node: &MethodNode,
    lambda: &Lambda,
    host: &Parameters,
    value_param_shift: u16,
) -> Result<MethodNode, InlineError> {
    let real_size: u16 = lambda
        .parameter_types
        .iter()
        .map(|ty| descriptors::size(ty) as u16)
        .sum();
    let words = |parameters: &[Parameter]| -> u16 {
        parameters
            .iter()
            .map(|parameter| parameter.category.words() as u16)
            .sum()
    };
    let captured_size = words(&host.captured[lambda.captured.clone()]);
    let host_captured_base = host.real_size() + words(&host.captured[..lambda.captured.start]);
    // `None` for a captured value, which keeps the body's slot and loses its entry.
    let place = |slot: u16| -> (u16, bool) {
        if slot < real_size {
            (slot + value_param_shift, true)
        } else if slot < real_size + captured_size {
            (host_captured_base + slot - real_size, false)
        } else {
            (slot - captured_size + value_param_shift, true)
        }
    };
    let mut out = node.clone();
    for entry in &mut out.nodes {
        if let Node::Insn(Insn::Var { slot, .. } | Insn::Iinc { slot, .. }) = entry {
            *slot = place(*slot).0;
        }
    }
    out.local_variables = node
        .local_variables
        .iter()
        .filter_map(|local| {
            let (slot, shifted) = place(local.slot);
            shifted.then(|| LocalVariable {
                slot,
                ..local.clone()
            })
        })
        .collect();
    Ok(out)
}
