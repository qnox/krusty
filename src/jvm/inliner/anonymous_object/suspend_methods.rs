//! The suspend methods of a regenerated object: kotlinc's `CoroutineTransformer` for an object an
//! inline function creates.
//!
//! kotlinc writes each suspend method of such an object twice: `m`, with its state machine for
//! callers that use the object as it was compiled, and `m$$forInline`, the body before the state
//! machine, its suspension points still between markers and its continuation built by a fake
//! constructor call between `mark(4)` and `mark(5)`. A copy keeps only the `$$forInline` body,
//! under the method's own name: the call's lambdas are inlined into it and the coroutine
//! transformer then builds the copy's own state machine over the result. When every suspension
//! point of the copy is a tail call, it needs no continuation, and the original's continuation
//! class is not copied at all. Otherwise the continuation is regenerated as the copy's next object,
//! with the machine's spill fields and `@DebugMetadata`.

use crate::jvm::bytecode_passes::coroutines::markers::{
    int_constant, is_suspend_inline_marker, SuspendMarker,
};
use crate::jvm::bytecode_passes::coroutines::{
    transform_named_function, transform_suspend_lambda, DebugMetadata, DeclaredSpillFields,
    SpillField, SuspendLambda, Transformed,
};
use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::class_node::{ClassMethod, ClassNode};
use crate::jvm::method_node::{Insn, MethodNode, Node};

use super::super::local_sorter::Sorter;
use super::RegenerationError;

/// kotlinc's `FOR_INLINE_SUFFIX`.
const FOR_INLINE_SUFFIX: &str = "$$forInline";
const NEW: u8 = 0xbb;
const GETFIELD: u8 = 0xb4;
/// `INVOKE_SUSPEND_METHOD_NAME`.
const INVOKE_SUSPEND: &str = "invokeSuspend";
/// The suspend lambda base classes (`SuspendLambda`, and `RestrictedSuspendLambda` for a lambda
/// with a restricted-suspension receiver).
const SUSPEND_LAMBDA_BASES: [&str; 2] = [
    "kotlin/coroutines/jvm/internal/SuspendLambda",
    "kotlin/coroutines/jvm/internal/RestrictedSuspendLambda",
];
/// The normalized descriptor of every reference spill field.
const OBJECT_DESCRIPTOR: &str = "Ljava/lang/Object;";
/// A suspend lambda's first local after `this` and `$result`.
const LAMBDA_FIRST_LOCAL: u16 = 2;
const ACC_STATIC: u16 = 0x0008;

/// The original's methods kotlinc keeps a `$$forInline` copy of.
pub(super) struct ForInlineCopies<'a> {
    copies: Vec<(&'a str, &'a str)>,
}

impl<'a> ForInlineCopies<'a> {
    pub(super) fn of(methods: &[&'a ClassMethod]) -> ForInlineCopies<'a> {
        ForInlineCopies {
            copies: methods
                .iter()
                .filter_map(|method| {
                    let name = method.name.strip_suffix(FOR_INLINE_SUFFIX)?;
                    Some((name, method.desc.as_str()))
                })
                .collect(),
        }
    }

    /// `shouldSkip`: `method` has a `$$forInline` copy, which the regenerated class takes instead.
    pub(super) fn skips(&self, method: &ClassMethod) -> bool {
        self.copies
            .iter()
            .any(|&(name, desc)| name == method.name && desc == method.desc)
    }
}

/// The name of the method `name` is the `$$forInline` copy of, when it is one.
pub(super) fn for_inline_original(name: &str) -> Option<&str> {
    name.strip_suffix(FOR_INLINE_SUFFIX)
}

/// `isInvokeSuspend` in a suspend lambda (`isContinuation`, not a `ContinuationImpl`): `method` is
/// the `invokeSuspend$$forInline` copy of a suspend lambda, whose machine is rebuilt with the
/// lambda as its own continuation (`newStateMachineForLambda`).
pub(super) fn is_lambda_machine(original: &ClassNode, method: &ClassMethod) -> bool {
    for_inline_original(&method.name) == Some(INVOKE_SUSPEND)
        && original
            .super_name
            .as_deref()
            .is_some_and(|name| SUSPEND_LAMBDA_BASES.contains(&name))
}

/// The spill fields of `owner` that `body`, a suspend lambda's `invokeSuspend$$forInline`, reads
/// its parameters back from (`L$0`, `I$0`, …), each kind with its highest index, by normalized
/// descriptor. The copy has no machine yet, so it reads no other spill field.
pub(super) fn parameter_spill_fields(body: &MethodNode, owner: &str) -> Vec<(String, usize)> {
    let mut declared: Vec<(String, usize)> = Vec::new();
    for insn in body.instructions() {
        let Insn::Field {
            op: GETFIELD,
            owner: field_owner,
            name,
            desc,
        } = insn
        else {
            continue;
        };
        if field_owner != owner {
            continue;
        }
        let Some(index) = spill_field_index(name) else {
            continue;
        };
        let normalized = if desc.starts_with('L') || desc.starts_with('[') {
            OBJECT_DESCRIPTOR
        } else {
            desc.as_str()
        };
        match declared.iter_mut().find(|(kind, _)| kind == normalized) {
            Some((_, max)) => *max = (*max).max(index),
            None => declared.push((normalized.to_string(), index)),
        }
    }
    declared
}

/// The index of a spill field named `X$n` (`fieldNameForVar`), when `name` is one.
fn spill_field_index(name: &str) -> Option<usize> {
    let (kind, index) = name.split_once('$')?;
    let mut kind = kind.chars();
    let letter = kind.next()?;
    if kind.next().is_some() || !letter.is_ascii_uppercase() || index.is_empty() {
        return None;
    }
    if !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    index.parse().ok()
}

/// `findFakeContinuationConstructorClassName`: the continuation class the body constructs right
/// after its `mark(4)`.
pub(super) fn fake_continuation_class(
    body: &MethodNode,
) -> Result<Option<&str>, RegenerationError> {
    let Some(marker) = body.nodes.windows(2).position(|pair| {
        is_suspend_inline_marker(&pair[1])
            && int_constant(&pair[0])
                == Some(SuspendMarker::BeforeFakeContinuationConstructorCall as i32)
    }) else {
        return Ok(None);
    };
    match body.nodes.get(marker + 2) {
        Some(Node::Insn(Insn::Type { op: NEW, class })) => Ok(Some(class)),
        _ => Err(RegenerationError::Unsupported(
            "a fake continuation marker not followed by its construction",
        )),
    }
}

/// Where the call that regenerates the object is: what the copy's state machine reports as its
/// own position (`InlineCallSiteInfo.file` and `lineNumber`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallSiteSource<'a> {
    /// The calling class's source file name.
    pub file: &'a str,
    /// The call's line as kotlinc's IR reports it to the transformer: zero-based.
    pub line: Option<u16>,
}

/// What a copied suspend method's state machine adds to its continuation: the spill fields and
/// the `@DebugMetadata` the transformer computed.
#[derive(Debug)]
pub(crate) struct ContinuationMachine {
    pub fields: Vec<SpillField>,
    pub debug_metadata: DebugMetadata,
}

/// `newStateMachineForNamedFunction` over `body`, the `$$forInline` copy of a method of `owner`
/// with the call's lambdas inlined and the fake continuation class renamed to `continuation`. The
/// machine `continuation` is regenerated for, or `None` when every suspension point is a tail call
/// and the copy needs no continuation.
pub(super) fn transform(
    body: MethodNode,
    owner: &str,
    continuation: &str,
    source: &CallSiteSource<'_>,
) -> Result<(MethodNode, Option<ContinuationMachine>), RegenerationError> {
    let completion_slot = last_parameter_slot(&body)?;
    let function = crate::jvm::bytecode_passes::coroutines::NamedFunction {
        owner,
        continuation_class: continuation,
        source_file: source.file,
        line_number: source.line,
        completion_slot,
        dispatch_receiver: Some(owner),
    };
    let (body, machine) =
        match transform_named_function(body, &function).map_err(RegenerationError::Coroutine)? {
            Transformed::TailCalls(body) => (body, None),
            Transformed::StateMachine(machine) => {
                let machine = *machine;
                (
                    machine.method,
                    Some(ContinuationMachine {
                        fields: machine.layout.fields,
                        debug_metadata: machine.debug_metadata,
                    }),
                )
            }
        };
    Ok((sorted_locals(body, completion_slot + 1), machine))
}

/// `newStateMachineForLambda` over `body`, the `invokeSuspend$$forInline` copy of the suspend
/// lambda `owner` with the call's lambdas inlined: the lambda is its own continuation, its machine
/// reusing the spill fields `declared` holds its parameters in. kotlinc attributes the machine's
/// own code to the call's line, as for a named function.
pub(super) fn transform_lambda(
    body: MethodNode,
    owner: &str,
    declared: &[(String, usize)],
    source: &CallSiteSource<'_>,
) -> Result<(MethodNode, ContinuationMachine), RegenerationError> {
    let line_number = source.line.ok_or(RegenerationError::Unsupported(
        "a suspend lambda regenerated for a call without a line",
    ))?;
    let declared_spill_fields: Vec<DeclaredSpillFields<'_>> = declared
        .iter()
        .map(|(descriptor, max_index)| DeclaredSpillFields {
            descriptor,
            max_index: *max_index,
        })
        .collect();
    let lambda = SuspendLambda {
        class: owner,
        source_file: source.file,
        line_number,
        declared_spill_fields: &declared_spill_fields,
    };
    let machine = transform_suspend_lambda(body, &lambda).map_err(RegenerationError::Coroutine)?;
    Ok((
        sorted_locals(machine.method, LAMBDA_FIRST_LOCAL),
        ContinuationMachine {
            fields: machine.layout.fields,
            debug_metadata: machine.debug_metadata,
        },
    ))
}

/// The copy as kotlinc's class builder writes it, through a `LocalVariablesSorter`: the machine's
/// own locals, which the transformer appended, take the slots after the parameters (`first_local`)
/// in the order the body first names them, ahead of the inlined lambda's.
fn sorted_locals(mut body: MethodNode, first_local: u16) -> MethodNode {
    let mut sorter = Sorter::new(first_local);
    for node in &mut body.nodes {
        if let Node::Insn(insn) = node {
            sorter.visit(insn);
        }
    }
    for local in &mut body.local_variables {
        sorter.visit_local(local);
    }
    body.max_locals = sorter.max_locals();
    body
}

/// `getLastParameterIndex`: the slot of the method's last parameter, its continuation.
fn last_parameter_slot(body: &MethodNode) -> Result<u16, RegenerationError> {
    let arguments = descriptors::argument_types(&body.desc)
        .filter(|arguments| !arguments.is_empty())
        .ok_or(RegenerationError::Unsupported(
            "a suspend method without a continuation parameter",
        ))?;
    let before: usize = arguments[..arguments.len() - 1]
        .iter()
        .map(|argument| descriptors::size(argument))
        .sum();
    let receiver = usize::from(body.access & ACC_STATIC == 0);
    u16::try_from(before + receiver).map_err(|_| {
        RegenerationError::Unsupported("a suspend method whose parameters exceed 65535 slots")
    })
}
