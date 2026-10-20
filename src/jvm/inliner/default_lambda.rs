//! An omitted inline lambda parameter's default, inlined from the lambda class a dependency
//! compiled for it: kotlinc's `DefaultLambda`.
//!
//! The `$default` bridge's arm loads the class's singleton ([`super::ExtractedDefaultLambda`]); the
//! call inlines the class's erased `invoke(Object…)Object` wherever the body invokes the
//! parameter (`loadDefaultLambdaBody`). When that `invoke` is the bridge to a typed one, the typed
//! body is copied into it (`inlineBridge`). The lambda object itself is the skipped `this` of the
//! body, which takes no slot of the caller, so the body is written without it: its parameters
//! start at slot 0, like those of a literal lambda's node. The body then gets the marker local
//! `$i$a$-<function>-<class>` (`createNodeWithFakeVariables`), and its reified operations are
//! specialized when the arm asked for it.
//!
//! A lambda the arm constructs takes its captured values as parameters after its own, the fields
//! its constructor assigns in assignment order (`findCapturedFieldAssignmentInstructions`): the
//! body's `aload 0; getfield` of one becomes a load of that parameter.

use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::class_node::ClassNode;
use crate::jvm::method_node::{Category, Insn, LabelId, LocalVariable, MethodNode, Node};
use crate::jvm::reified_arguments::ReifiedArguments;

use super::default_masks::DEFAULT_LAMBDA_FAKE_CALL;
use super::{ExtractedDefaultLambda, InlineError, Lambda, Parameters};

const ICONST_0: u8 = 0x03;
const ISTORE: u8 = 0x36;
const ALOAD: u8 = 0x19;
const GOTO: u8 = 0xa7;
const GETFIELD: u8 = 0xb4;
const PUTFIELD: u8 = 0xb5;
const ILOAD: u8 = 0x15;
const ICONST_M1: u8 = 0x02;
const ICONST_5: u8 = 0x08;
const BIPUSH: u8 = 0x10;
const SIPUSH: u8 = 0x11;
const INVOKESPECIAL: u8 = 0xb7;
const RETURN: u8 = 0xb1;
const INVOKESTATIC: u8 = 0xb8;
const ACC_STATIC: u16 = 0x0008;
const ACC_BRIDGE: u16 = 0x0040;
const OBJECT: &str = "Ljava/lang/Object;";

/// The super classes of the lambda classes a default lambda cannot be: a suspend lambda's body is
/// its state machine, and a callable reference reads its target through its own members.
const UNSUPPORTED_SUPER_CLASSES: [&str; 13] = [
    "kotlin/coroutines/jvm/internal/SuspendLambda",
    "kotlin/coroutines/jvm/internal/RestrictedSuspendLambda",
    "kotlin/jvm/internal/FunctionReference",
    "kotlin/jvm/internal/FunctionReferenceImpl",
    "kotlin/jvm/internal/AdaptedFunctionReference",
    "kotlin/jvm/internal/PropertyReference0",
    "kotlin/jvm/internal/PropertyReference1",
    "kotlin/jvm/internal/PropertyReference2",
    "kotlin/jvm/internal/MutablePropertyReference0",
    "kotlin/jvm/internal/MutablePropertyReference1",
    "kotlin/jvm/internal/MutablePropertyReference2",
    "kotlin/jvm/internal/PropertyReference0Impl",
    "kotlin/jvm/internal/PropertyReference1Impl",
];

/// The lambda `extracted` names, read from its class: the body every `invoke` of the parameter
/// is replaced with. `function` is the inline function's name, without `$default`.
pub(in crate::jvm) fn load(
    class: &ClassNode,
    extracted: &ExtractedDefaultLambda,
    function: &str,
    reified: &ReifiedArguments,
) -> Result<Lambda, InlineError> {
    if class
        .super_name
        .as_deref()
        .is_some_and(|super_name| UNSUPPORTED_SUPER_CLASSES.contains(&super_name))
    {
        return Err(unsupported(
            "a default lambda that is not a plain lambda class",
        ));
    }
    let erased = class
        .methods
        .iter()
        .find(|method| {
            method.name == "invoke"
                && descriptors::return_type(&method.desc) == Some(OBJECT)
                && descriptors::argument_types(&method.desc)
                    .is_some_and(|arguments| arguments.iter().all(|argument| *argument == OBJECT))
        })
        .ok_or_else(|| malformed("a default lambda class has no erased invoke"))?;
    if erased.access & ACC_STATIC != 0 {
        return Err(malformed("a default lambda's invoke is static"));
    }
    let mut node = erased
        .code
        .clone()
        .ok_or_else(|| malformed("a default lambda's invoke has no code"))?;
    if erased.access & ACC_BRIDGE != 0 {
        inline_bridge(&mut node, class)?;
    }
    let captured = match &extracted.constructor {
        Some(constructor) => captured_fields(class, constructor)?,
        None => Vec::new(),
    };
    read_captured_parameters(&mut node, class, &captured)?;
    drop_receiver(&mut node)?;
    add_marker_variable(&mut node, function, &class.name);
    if extracted.need_reification {
        super::reified::specialize(
            &mut node,
            reified,
            &mut crate::jvm::reified_arguments::NoEnumEntriesReads,
        )?;
    }
    if node.instructions().any(is_class_reification_marker) {
        return Err(unsupported(
            "a default lambda whose objects need reification",
        ));
    }
    let parameter_types: Vec<String> = descriptors::argument_types(&node.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?
        .into_iter()
        .map(str::to_string)
        .collect();
    let return_type = descriptors::return_type(&node.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?
        .to_string();
    node.access |= ACC_STATIC;
    let close = node
        .desc
        .rfind(')')
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?;
    let captured_descriptors: String = captured.iter().map(|field| field.desc.as_str()).collect();
    node.desc.insert_str(close, &captured_descriptors);
    Ok(Lambda {
        value_class_parameters: vec![None; parameter_types.len()],
        parameter_types,
        return_type,
        value_class_return: None,
        captured: 0..0,
        capture_names: captured.into_iter().map(|field| Some(field.name)).collect(),
        default_class: Some(class.name.clone()),
        node,
    })
}

/// Each fake call a default arm left where it constructed its lambda becomes the stores of the
/// values it takes into the lambda's captured parameters, the last value first, as kotlinc's
/// `MethodInliner` writes them in place of `DEFAULT_LAMBDA_FAKE_CALL`. The body is the prepared
/// one, whose captured parameters follow its own.
pub(super) fn store_captured_values(
    node: &mut MethodNode,
    parameters: &Parameters,
    lambdas: &[Lambda],
) -> Result<(), InlineError> {
    let mut at = 0;
    while at < node.nodes.len() {
        let Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner,
            name,
            desc,
            ..
        }) = &node.nodes[at]
        else {
            at += 1;
            continue;
        };
        if owner != DEFAULT_LAMBDA_FAKE_CALL {
            at += 1;
            continue;
        }
        let slot: usize = name
            .strip_prefix(DEFAULT_LAMBDA_FAKE_CALL)
            .and_then(|slot| slot.parse().ok())
            .ok_or_else(|| malformed("a default lambda's fake call names no parameter"))?;
        let lambda = parameters
            .lambda_at(slot)
            .and_then(|lambda| lambdas.get(lambda))
            .ok_or_else(|| malformed("a default lambda's fake call names no inlined lambda"))?;
        let types = descriptors::argument_types(desc)
            .ok_or_else(|| malformed("a default lambda's fake call cannot be parsed"))?;
        if types.len() != lambda.captured.len() {
            return Err(malformed(
                "a default lambda's fake call does not take its captured values",
            ));
        }
        let mut slot = parameters.real_size()
            + parameters.captured[..lambda.captured.start]
                .iter()
                .map(|parameter| parameter.category.words() as u16)
                .sum::<u16>();
        let mut stores = Vec::with_capacity(types.len());
        for ty in &types {
            stores.push(Node::Insn(Insn::Var {
                op: descriptors::typed_opcode(ty, ISTORE),
                slot,
            }));
            slot += descriptors::size(ty) as u16;
        }
        stores.reverse();
        let count = stores.len();
        node.nodes.splice(at..=at, stores);
        at += count;
    }
    Ok(())
}

/// A field a lambda's constructor assigns from one of its parameters.
struct CapturedField {
    name: String,
    desc: String,
}

/// The fields `class`'s constructor `constructor` assigns its parameters to, in assignment order:
/// `aload 0; <load>; putfield` (`findCapturedFieldAssignmentInstructions`). They must take the
/// constructor's parameters in order, which the arm left on the stack.
fn captured_fields(
    class: &ClassNode,
    constructor: &str,
) -> Result<Vec<CapturedField>, InlineError> {
    let code = class
        .methods
        .iter()
        .find(|method| method.name == "<init>" && method.desc == constructor)
        .ok_or_else(|| malformed("a default lambda's arm calls a constructor its class lacks"))?
        .code
        .as_ref()
        .ok_or_else(|| malformed("a default lambda's constructor has no code"))?;
    let super_name = class
        .super_name
        .as_deref()
        .ok_or_else(|| malformed("a default lambda class has no superclass"))?;
    parameter_assignments(&class.name, super_name, code)
}

/// The fields of `owner` that the constructor `code` assigns from its parameters. The constructor
/// must be exactly kotlinc's lambda constructor: one `aload 0; <load>; putfield` per parameter in
/// declaration order, each loading that parameter's own slot by the load its type takes into a
/// distinct field of its type, then `aload 0; [arity]; invokespecial <super>.<init>; return`.
/// Anything else is refused, since deleting the constructor would drop its other work or bind a
/// capture to the wrong field.
fn parameter_assignments(
    owner: &str,
    super_name: &str,
    code: &MethodNode,
) -> Result<Vec<CapturedField>, InlineError> {
    let refused =
        || unsupported("a default lambda whose constructor is not kotlinc's lambda constructor");
    let parameters = descriptors::argument_types(&code.desc)
        .ok_or_else(|| malformed("a default lambda's constructor descriptor cannot be parsed"))?;
    let mut instructions = code.instructions();
    let mut expected_slot = 1;
    let mut fields: Vec<CapturedField> = Vec::with_capacity(parameters.len());
    for parameter in &parameters {
        let (
            Some(Insn::Var { op: ALOAD, slot: 0 }),
            Some(Insn::Var { op: load, slot }),
            Some(Insn::Field {
                op: PUTFIELD,
                owner: field_owner,
                name,
                desc,
            }),
        ) = (
            instructions.next(),
            instructions.next(),
            instructions.next(),
        )
        else {
            return Err(refused());
        };
        if field_owner != owner
            || desc != parameter
            || *slot != expected_slot
            || *load != Category::of_descriptor(parameter).load_op()
            || fields.iter().any(|field| field.name == *name)
        {
            return Err(refused());
        }
        expected_slot += descriptors::size(parameter) as u16;
        fields.push(CapturedField {
            name: name.clone(),
            desc: desc.clone(),
        });
    }
    if !matches!(instructions.next(), Some(Insn::Var { op: ALOAD, slot: 0 })) {
        return Err(refused());
    }
    let mut next = instructions.next();
    let arity = matches!(
        next,
        Some(
            Insn::Op(ICONST_M1..=ICONST_5)
                | Insn::Int {
                    op: BIPUSH | SIPUSH,
                    ..
                }
        )
    );
    if arity {
        next = instructions.next();
    }
    let super_constructor = if arity { "(I)V" } else { "()V" };
    let Some(Insn::Method {
        op: INVOKESPECIAL,
        owner: called,
        name,
        desc,
        ..
    }) = next
    else {
        return Err(refused());
    };
    if called != super_name
        || name != "<init>"
        || desc != super_constructor
        || instructions.next() != Some(&Insn::Op(RETURN))
        || instructions.next().is_some()
    {
        return Err(refused());
    }
    Ok(fields)
}

/// The captured values become parameters after the body's own, its locals moving up by their
/// words, and each `aload 0; getfield` of one becomes a load of its parameter.
fn read_captured_parameters(
    node: &mut MethodNode,
    class: &ClassNode,
    captured: &[CapturedField],
) -> Result<(), InlineError> {
    if captured.is_empty() {
        return Ok(());
    }
    let first = 1 + descriptors::argument_types(&node.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?
        .iter()
        .map(|argument| descriptors::size(argument) as u16)
        .sum::<u16>();
    let mut slots = Vec::with_capacity(captured.len());
    let mut next = first;
    for field in captured {
        slots.push(next);
        next += descriptors::size(&field.desc) as u16;
    }
    let shift = next - first;
    for entry in &mut node.nodes {
        if let Node::Insn(Insn::Var { slot, .. } | Insn::Iinc { slot, .. }) = entry {
            if *slot >= first {
                *slot += shift;
            }
        }
    }
    for local in &mut node.local_variables {
        if local.slot >= first {
            local.slot += shift;
        }
    }
    node.max_locals += shift;
    let mut at = 0;
    while at + 1 < node.nodes.len() {
        let read = match (&node.nodes[at], &node.nodes[at + 1]) {
            (
                Node::Insn(Insn::Var { op: ALOAD, slot: 0 }),
                Node::Insn(Insn::Field {
                    op: GETFIELD,
                    owner,
                    name,
                    desc,
                }),
            ) if *owner == class.name => captured
                .iter()
                .position(|field| field.name == *name && field.desc == *desc),
            _ => None,
        };
        if let Some(field) = read {
            node.nodes[at] = Node::Insn(Insn::Var {
                op: descriptors::typed_opcode(&captured[field].desc, ILOAD),
                slot: slots[field],
            });
            node.nodes.remove(at + 1);
        }
        at += 1;
    }
    Ok(())
}

/// `inlineBridge`: the erased `invoke` only adapts its arguments for the typed one it calls on
/// `this`, so the typed body replaces that call, its parameters stored from the adapted values and
/// its returns jumping to what the bridge does with the result.
fn inline_bridge(bridge: &mut MethodNode, class: &ClassNode) -> Result<(), InlineError> {
    let mut calls = bridge.nodes.iter().enumerate().filter(|(_, entry)| {
        matches!(entry, Node::Insn(Insn::Method { owner, .. }) if *owner == class.name)
    });
    let (call, entry) = calls
        .next()
        .ok_or_else(|| malformed("a default lambda's bridge calls no invoke of its class"))?;
    if calls.next().is_some() {
        return Err(malformed(
            "a default lambda's bridge calls its class more than once",
        ));
    }
    let Node::Insn(Insn::Method { name, desc, .. }) = entry else {
        unreachable!("the call is a method instruction")
    };
    let target = class
        .methods
        .iter()
        .find(|method| method.name == *name && method.desc == *desc)
        .ok_or_else(|| malformed("a default lambda's bridge calls an invoke its class lacks"))?;
    if target.access & ACC_STATIC != 0 {
        return Err(unsupported("a default lambda whose typed invoke is static"));
    }
    let typed = target
        .code
        .as_ref()
        .ok_or_else(|| malformed("a default lambda's typed invoke has no code"))?;
    let arguments = descriptors::argument_types(&target.desc)
        .ok_or_else(|| malformed("a default lambda's descriptor cannot be parsed"))?;
    let arguments_size = arguments
        .iter()
        .map(|argument| descriptors::size(argument) as u16)
        .sum::<u16>()
        + 1;

    // The typed body's labels join the bridge's.
    let mut labels = Vec::with_capacity(typed.label_count());
    for _ in 0..typed.label_count() {
        labels.push(bridge.new_label());
    }
    let label = |label: LabelId| labels[label.index()];
    let invoke_label = bridge.new_label();
    let return_label = bridge.new_label();

    let mut inserted = Vec::new();
    let mut offset = arguments_size;
    for argument in arguments.iter().rev() {
        offset -= descriptors::size(argument) as u16;
        inserted.push(Node::Insn(Insn::Var {
            op: descriptors::typed_opcode(argument, ISTORE),
            slot: offset,
        }));
    }
    // The bridge loads `this` as the receiver of the call it makes. kotlinc pops it here, then
    // deletes both as moves of the skipped lambda parameter; neither is written.
    inserted.push(Node::Label(invoke_label));
    for entry in &typed.nodes {
        inserted.push(match entry {
            Node::Label(original) => Node::Label(label(*original)),
            Node::Line { line, start } => Node::Line {
                line: *line,
                start: label(*start),
            },
            Node::Insn(Insn::Op(0xac..=0xb1)) => Node::Insn(Insn::Jump {
                op: GOTO,
                target: return_label,
            }),
            Node::Insn(insn) => Node::Insn(relabel(insn, &label)),
        });
    }
    inserted.push(Node::Label(return_label));
    let receiver = bridge
        .nodes
        .iter()
        .position(|entry| matches!(entry, Node::Insn(Insn::Var { op: ALOAD, slot: 0 })))
        .filter(|&at| at < call)
        .ok_or_else(|| malformed("a default lambda's bridge does not load its receiver"))?;
    let mut nodes = Vec::with_capacity(bridge.nodes.len() + inserted.len());
    for (at, entry) in std::mem::take(&mut bridge.nodes).into_iter().enumerate() {
        if at == call {
            nodes.append(&mut inserted);
        } else if at != receiver && !matches!(entry, Node::Line { .. }) {
            nodes.push(entry);
        }
    }
    bridge.nodes = nodes;
    bridge.local_variables = typed
        .local_variables
        .iter()
        .map(|local| {
            let (start, end) = if local.slot < arguments_size {
                (invoke_label, return_label)
            } else {
                (label(local.start), label(local.end))
            };
            LocalVariable {
                start,
                end,
                ..local.clone()
            }
        })
        .collect();
    bridge.try_catch_blocks = typed
        .try_catch_blocks
        .iter()
        .map(|block| crate::jvm::method_node::TryCatchBlock {
            start: label(block.start),
            end: label(block.end),
            handler: label(block.handler),
            catch_type: block.catch_type.clone(),
        })
        .collect();
    bridge.max_locals = bridge.max_locals.max(typed.max_locals);
    bridge.max_stack = bridge.max_stack.max(typed.max_stack);
    Ok(())
}

/// `insn` with its labels renamed by `label`.
fn relabel(insn: &Insn, label: &dyn Fn(LabelId) -> LabelId) -> Insn {
    match insn {
        Insn::Jump { op, target } => Insn::Jump {
            op: *op,
            target: label(*target),
        },
        Insn::TableSwitch {
            low,
            high,
            default,
            labels,
        } => Insn::TableSwitch {
            low: *low,
            high: *high,
            default: label(*default),
            labels: labels.iter().map(|target| label(*target)).collect(),
        },
        Insn::LookupSwitch {
            default,
            keys,
            labels,
        } => Insn::LookupSwitch {
            default: label(*default),
            keys: keys.clone(),
            labels: labels.iter().map(|target| label(*target)).collect(),
        },
        other => other.clone(),
    }
}

/// The skipped `this` takes no slot: every other local moves down one word. A body that still
/// reads the lambda object is not inlined yet.
fn drop_receiver(node: &mut MethodNode) -> Result<(), InlineError> {
    for entry in &mut node.nodes {
        if let Node::Insn(Insn::Var { slot, .. } | Insn::Iinc { slot, .. }) = entry {
            *slot = slot
                .checked_sub(1)
                .ok_or_else(|| unsupported("a default lambda that reads its own object"))?;
        }
    }
    node.local_variables.retain(|local| local.slot != 0);
    for local in &mut node.local_variables {
        local.slot -= 1;
    }
    node.max_locals = node.max_locals.saturating_sub(1);
    Ok(())
}

/// `createNodeWithFakeVariables`: the marker local kotlinc names an inlined lambda by, set before
/// the body and live to its end.
fn add_marker_variable(node: &mut MethodNode, function: &str, class: &str) {
    let slot = node.max_locals;
    let start = node.new_label();
    node.nodes.splice(
        0..0,
        [
            Node::Insn(Insn::Op(ICONST_0)),
            Node::Insn(Insn::Var { op: ISTORE, slot }),
            Node::Label(start),
        ],
    );
    let end = match node.nodes.last() {
        Some(Node::Label(end)) => *end,
        _ => {
            let end = node.new_label();
            node.nodes.push(Node::Label(end));
            end
        }
    };
    let simple = class.rsplit('/').next().unwrap_or(class);
    node.local_variables.push(LocalVariable {
        name: format!("$i$a$-{function}-{simple}"),
        desc: "I".to_string(),
        start,
        end,
        slot,
    });
    node.max_locals += 1;
}

fn is_class_reification_marker(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Method { op: INVOKESTATIC, owner, name, .. }
            if owner == "kotlin/jvm/internal/Intrinsics" && name == "needClassReification"
    )
}

fn unsupported(reason: &'static str) -> InlineError {
    InlineError::DefaultLambda(reason)
}

fn malformed(detail: &str) -> InlineError {
    InlineError::Analysis(detail.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LLOAD: u8 = 0x16;
    const DLOAD: u8 = 0x18;
    const OWNER: &str = "Client$f$1";
    const LAMBDA: &str = "kotlin/jvm/internal/Lambda";

    fn assign(node: &mut MethodNode, op: u8, slot: u16, field: &str, desc: &str) {
        node.nodes
            .push(Node::Insn(Insn::Var { op: ALOAD, slot: 0 }));
        node.nodes.push(Node::Insn(Insn::Var { op, slot }));
        node.nodes.push(Node::Insn(Insn::Field {
            op: PUTFIELD,
            owner: OWNER.into(),
            name: field.into(),
            desc: desc.into(),
        }));
    }

    fn super_call(node: &mut MethodNode) {
        node.nodes
            .push(Node::Insn(Insn::Var { op: ALOAD, slot: 0 }));
        node.nodes.push(Node::Insn(Insn::Op(ICONST_0)));
        node.nodes.push(Node::Insn(Insn::Method {
            op: INVOKESPECIAL,
            owner: LAMBDA.into(),
            name: "<init>".into(),
            desc: "(I)V".into(),
            interface: false,
        }));
        node.nodes.push(Node::Insn(Insn::Op(RETURN)));
    }

    /// kotlinc's constructor `(desc)V` for `loads`, assigned in turn to fields `a`, `b`, ….
    fn constructor(desc: &str, loads: &[(u8, u16, &str)]) -> MethodNode {
        let mut node = MethodNode::new(0, "<init>", desc);
        for (index, (op, slot, field_desc)) in loads.iter().enumerate() {
            let field = ((b'a' + index as u8) as char).to_string();
            assign(&mut node, *op, *slot, &field, field_desc);
        }
        super_call(&mut node);
        node
    }

    fn names(fields: &[CapturedField]) -> Vec<&str> {
        fields.iter().map(|field| field.name.as_str()).collect()
    }

    fn refused(code: &MethodNode) -> bool {
        matches!(
            parameter_assignments(OWNER, LAMBDA, code),
            Err(InlineError::DefaultLambda(_))
        )
    }

    #[test]
    fn a_constructor_assigning_its_parameters_in_order_captures_them() {
        let code = constructor(
            "(JLjava/lang/String;I)V",
            &[
                (LLOAD, 1, "J"),
                (ALOAD, 3, "Ljava/lang/String;"),
                (ILOAD, 4, "I"),
            ],
        );
        let fields = parameter_assignments(OWNER, LAMBDA, &code).expect("kotlinc's constructor");
        assert_eq!(names(&fields), ["a", "b", "c"]);
    }

    #[test]
    fn a_constructor_assigning_same_typed_parameters_out_of_order_is_refused() {
        assert!(refused(&constructor(
            "(II)V",
            &[(ILOAD, 2, "I"), (ILOAD, 1, "I")]
        )));
    }

    #[test]
    fn a_constructor_assigning_one_parameter_twice_is_refused() {
        assert!(refused(&constructor(
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[
                (ALOAD, 1, "Ljava/lang/String;"),
                (ALOAD, 1, "Ljava/lang/String;"),
            ],
        )));
    }

    #[test]
    fn a_two_word_parameter_moves_the_next_parameter_two_slots() {
        assert!(refused(&constructor(
            "(DI)V",
            &[(DLOAD, 1, "D"), (ILOAD, 2, "I")]
        )));
        let code = constructor("(DI)V", &[(DLOAD, 1, "D"), (ILOAD, 3, "I")]);
        let fields = parameter_assignments(OWNER, LAMBDA, &code).expect("double then int");
        assert_eq!(names(&fields), ["a", "b"]);
    }

    #[test]
    fn a_constructor_assigning_two_parameters_to_one_field_is_refused() {
        let mut code = MethodNode::new(0, "<init>", "(II)V");
        assign(&mut code, ILOAD, 1, "a", "I");
        assign(&mut code, ILOAD, 2, "a", "I");
        super_call(&mut code);
        assert!(refused(&code));
    }

    #[test]
    fn a_constructor_that_writes_a_capture_again_later_is_refused() {
        let mut code = constructor("(I)V", &[(ILOAD, 1, "I")]);
        let ret = code.nodes.pop().expect("return");
        assign(&mut code, ILOAD, 1, "a", "I");
        code.nodes.push(ret);
        assert!(refused(&code));
    }

    #[test]
    fn a_constructor_with_other_work_is_refused() {
        let mut code = constructor("(I)V", &[(ILOAD, 1, "I")]);
        code.nodes.insert(
            3,
            Node::Insn(Insn::Method {
                op: 0xb8,
                owner: "Side".into(),
                name: "effect".into(),
                desc: "()V".into(),
                interface: false,
            }),
        );
        assert!(refused(&code));
    }
}
