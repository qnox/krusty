//! Abstract interpretation of a compiled inline body, tracking where each value came from.
//!
//! The recogniser used to read a call's operands by scanning backwards for LOCAL loads, which can
//! only see values that pass through a local. `runCatching` hands its lambda's result straight to
//! the wrapper that builds a `Result` and never stores it, so a backward local scan sees nothing at
//! all there. Simulating the body forwards over an abstract stack names every operand regardless of
//! whether it was spilled, and the previous local-only decode falls out as the case where every
//! operand happens to be a parameter.
//!
//! The domain is deliberately tiny: a value is a parameter, the lambda's invocation result, the
//! result of an earlier call, the throwable that entered a handler, or [`Value::Opaque`] — anything
//! the plan cannot name. An operand that reaches a call as `Opaque` is what makes the whole body
//! undecodable, which is how constants, field reads and unmodelled opcodes decline safely.

use crate::jvm::inline::{loaded_local, stored_local, Insn};

/// Where a value on the abstract stack came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Value {
    /// A parameter of the inline declaration, by ordinal.
    Parameter(usize),
    /// The result of invoking the function-typed parameter.
    Invocation,
    /// The result of the call at this index in the simulated block's own call list.
    Call(usize),
    /// The throwable that entered the exception handler being simulated.
    Cause,
    /// A value this domain cannot name. Reaching a call as an operand declines the body.
    Opaque,
}

/// One non-intrinsic call the body makes, with every operand resolved to a [`Value`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SimulatedCall<'a> {
    pub(super) target: (&'a str, &'a str, &'a str, bool),
    /// `true` for `invokestatic` — a top-level function rather than a call on a receiver.
    pub(super) is_static: bool,
    /// Receiver first (for a non-static call), then one per declared parameter.
    pub(super) operands: Vec<Value>,
}

/// The one invocation of the function-typed parameter a body performs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SimulatedInvocation {
    pub(super) lambda_parameter: usize,
    pub(super) arguments: Vec<Value>,
    /// How many calls the block had already made when the invocation happened, so the block's calls
    /// split into what ran before it and what ran after.
    pub(super) after: usize,
}

/// What one simulated straight-line block produced.
#[derive(Debug)]
pub(super) struct SimulatedBlock<'a> {
    pub(super) calls: Vec<SimulatedCall<'a>>,
    /// The value the block returns, if it ends in a value-returning `return`.
    pub(super) returned: Option<Value>,
    /// The invocation of the function-typed parameter, absent in a handler block.
    pub(super) invocation: Option<SimulatedInvocation>,
}

/// Simulation inputs that do not change between the blocks of one body.
pub(super) struct BodyShape<'a> {
    pub(super) instructions: &'a [Insn],
    pub(super) source_cp: &'a [crate::jvm::classreader::C],
    /// Local slot of each declared parameter, by ordinal.
    pub(super) parameter_slots: &'a [u16],
    /// The local a body uses to remember the throwable that left its invocation, when it has one.
    /// Seeding it lets both the normal exit and the handler name that value the same way.
    pub(super) cause_slot: Option<u16>,
}

impl BodyShape<'_> {
    fn parameter_at(&self, slot: u16) -> Option<usize> {
        self.parameter_slots
            .iter()
            .position(|candidate| *candidate == slot)
    }
}

/// Simulate one straight-line block, starting at `entry` with `stack` already holding what the JVM
/// would have put there (empty for the body's own entry, the caught throwable for a handler).
///
/// Following an unconditional forward `goto` is the only control flow modelled; anything that
/// branches, loops or otherwise leaves the straight line declines by returning `None`, because a
/// plan asserts that its calls all run.
pub(super) fn simulate<'a>(
    shape: &BodyShape<'a>,
    entry: usize,
    stack: Vec<Value>,
) -> Option<SimulatedBlock<'a>> {
    let mut locals = std::collections::HashMap::new();
    if let Some(cause_slot) = shape.cause_slot {
        locals.insert(cause_slot, Value::Cause);
    }
    let mut state = State {
        stack,
        locals,
        calls: Vec::new(),
        invocation: None,
    };
    let mut index = entry;
    while let Some(instruction) = shape.instructions.get(index) {
        match state.step(shape, instruction)? {
            Step::Next => index += 1,
            Step::Jump(target) => {
                // A backward jump is a loop, which a straight-line block cannot describe.
                if target <= index {
                    return None;
                }
                index = target;
            }
            Step::Return(returned) => {
                return Some(SimulatedBlock {
                    calls: state.calls,
                    returned,
                    invocation: state.invocation,
                })
            }
        }
    }
    None
}

enum Step {
    Next,
    Jump(usize),
    Return(Option<Value>),
}

struct State<'a> {
    stack: Vec<Value>,
    locals: std::collections::HashMap<u16, Value>,
    calls: Vec<SimulatedCall<'a>>,
    invocation: Option<SimulatedInvocation>,
}

impl<'a> State<'a> {
    fn pop(&mut self) -> Option<Value> {
        self.stack.pop()
    }

    fn step(&mut self, shape: &BodyShape<'a>, instruction: &Insn) -> Option<Step> {
        if let Some(slot) = loaded_local(instruction) {
            let value = match shape.parameter_at(slot) {
                Some(parameter) => Value::Parameter(parameter),
                None => self.locals.get(&slot).copied().unwrap_or(Value::Opaque),
            };
            self.stack.push(value);
            return Some(Step::Next);
        }
        if let Some(slot) = stored_local(instruction) {
            let value = self.pop()?;
            // The cause local is that value by definition: the body initializes it to `null` before
            // the invocation and overwrites it with the caught throwable in the handler, and the
            // plan models both as the one value a cleanup reads. Letting either store win would
            // erase it — the `null` on the normal exit is what a backward scan used to see.
            if shape.cause_slot != Some(slot) {
                self.locals.insert(slot, value);
            }
            return Some(Step::Next);
        }
        if let Some(target) = crate::jvm::inline::invoked_method(instruction, shape.source_cp) {
            return self.call(instruction, target);
        }
        match instruction {
            // An unconditional jump keeps the block straight; every other branch shape declines.
            Insn::Branch {
                op: 0xa7,
                target: crate::jvm::inline::BranchTarget::Internal(target),
            }
            | Insn::BranchW {
                op: 0xc8,
                target: crate::jvm::inline::BranchTarget::Internal(target),
            } => Some(Step::Jump(*target)),
            Insn::Branch { .. }
            | Insn::BranchW { .. }
            | Insn::TableSwitch { .. }
            | Insn::LookupSwitch { .. } => None,
            Insn::Plain { op, .. } => match *op {
                0x00 => Some(Step::Next),               // nop
                0x57 => self.pop().map(|_| Step::Next), // pop
                0x58 => {
                    // pop2 — these bodies only ever discard one category-2 value or two slots.
                    self.pop()?;
                    self.pop();
                    Some(Step::Next)
                }
                0x59 => {
                    let top = *self.stack.last()?; // dup
                    self.stack.push(top);
                    Some(Step::Next)
                }
                0xc0 | 0xc1 => Some(Step::Next), // checkcast / instanceof leave the operand's origin
                0xb1 => Some(Step::Return(None)), // return
                0xac..=0xb0 => Some(Step::Return(Some(self.pop()?))), // *return
                0xbf => Some(Step::Return(None)), // athrow — the block leaves without a value
                // Everything that produces a value this domain cannot name pushes `Opaque`, which
                // declines only if it actually reaches a call.
                0x01..=0x0f | 0x12..=0x14 | 0xb2 | 0xb4 => {
                    self.stack.push(Value::Opaque);
                    Some(Step::Next)
                }
                _ => None,
            },
        }
    }

    fn call(
        &mut self,
        instruction: &Insn,
        target: (&'a str, &'a str, &'a str, bool),
    ) -> Option<Step> {
        let Insn::Plain { op, .. } = instruction else {
            return None;
        };
        let is_static = *op == 0xb8;
        let (parameters, ret) = crate::jvm::ir_emit::parse_physical_method_desc(target.2)?;
        let mut operands = Vec::with_capacity(parameters.len() + 1);
        for _ in 0..parameters.len() {
            operands.push(self.pop()?);
        }
        if !is_static {
            operands.push(self.pop()?);
        }
        operands.reverse();
        // The function-typed parameter's own invocation is the body's centre, not one of its calls.
        // A body that invokes twice is some other shape; one plan describes one invocation.
        if target.0.starts_with("kotlin/jvm/functions/") && target.1 == "invoke" {
            if self.invocation.is_some() {
                return None;
            }
            let (&receiver, arguments) = operands.split_first()?;
            let Value::Parameter(lambda_parameter) = receiver else {
                return None;
            };
            self.invocation = Some(SimulatedInvocation {
                lambda_parameter,
                arguments: arguments.to_vec(),
                after: self.calls.len(),
            });
            self.stack.push(Value::Invocation);
            return Some(Step::Next);
        }
        // Inlining markers and null checks carry no value and are not part of any plan.
        if target.0.starts_with("kotlin/jvm/internal/") {
            if ret != crate::types::Ty::Unit {
                self.stack.push(Value::Opaque);
            }
            return Some(Step::Next);
        }
        self.calls.push(SimulatedCall {
            target,
            is_static,
            operands,
        });
        if ret != crate::types::Ty::Unit {
            self.stack.push(Value::Call(self.calls.len() - 1));
        }
        Some(Step::Next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate one stdlib body and report, for each block, the calls it makes with their resolved
    /// operands and the value it returns. This is the recogniser's whole view of a body, so a test
    /// comparing it whole is comparing exactly what decoding depends on.
    /// One decoded call, as a test compares it: the declaration it names, whether it is static, and
    /// where each of its operands came from.
    type ReportedCall = (String, bool, Vec<Value>);

    /// One simulated block: its calls, the value it returns, and the invocation it performed.
    type ReportedBlock = (
        Vec<ReportedCall>,
        Option<Value>,
        Option<SimulatedInvocation>,
    );

    fn simulated(
        jar: std::path::PathBuf,
        owner: &str,
        name: &str,
        descriptor: &str,
        parameter_slots: &[u16],
        cause_slot: Option<u16>,
    ) -> Vec<ReportedBlock> {
        let cp = crate::jvm::classpath::Classpath::new(vec![jar]);
        let body = cp
            .method_code(owner, name, descriptor)
            .expect("the declaration publishes a body");
        let instructions = crate::jvm::inline::disassemble(&body.code).expect("disassembles");
        let shape = BodyShape {
            instructions: &instructions,
            source_cp: &body.source_cp,
            parameter_slots,
            cause_slot,
        };
        // The body's own entry, then one block per `Throwable` handler, entered with the caught
        // value already on the stack exactly as the JVM leaves it.
        let mut entries = vec![(0usize, Vec::new())];
        // Several exception-table rows can share one handler, so simulate each handler block once.
        let mut seen = std::collections::HashSet::new();
        for handler in body
            .handlers
            .iter()
            .filter(|handler| seen.insert(handler.handler_pc))
        {
            let offsets = crate::jvm::inline::insn_offsets_at(&instructions, 0);
            let Some(handler_index) = offsets
                .iter()
                .position(|offset| *offset == handler.handler_pc as usize)
            else {
                continue;
            };
            entries.push((handler_index, vec![Value::Cause]));
        }
        entries
            .into_iter()
            .filter_map(|(entry, stack)| simulate(&shape, entry, stack))
            .map(|block| {
                (
                    block
                        .calls
                        .iter()
                        .map(|call| {
                            (
                                format!("{}.{}", call.target.0, call.target.1),
                                call.is_static,
                                call.operands.clone(),
                            )
                        })
                        .collect(),
                    block.returned,
                    block.invocation,
                )
            })
            .collect()
    }

    /// `runCatching` is the body a backward local scan cannot read: the invocation's result goes
    /// straight into the wrapper on the stack and is never stored. Forward simulation names it.
    #[test]
    fn run_catching_threads_its_invocation_through_the_stack() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        assert_eq!(
            simulated(
                stdlib,
                "kotlin/ResultKt",
                "runCatching",
                "(Lkotlin/jvm/functions/Function0;)Ljava/lang/Object;",
                &[0],
                None,
            ),
            vec![
                (
                    vec![(
                        "kotlin/Result.constructor-impl".to_owned(),
                        true,
                        vec![Value::Invocation]
                    )],
                    Some(Value::Call(0)),
                    Some(SimulatedInvocation {
                        lambda_parameter: 0,
                        arguments: Vec::new(),
                        after: 0,
                    }),
                ),
                (
                    vec![
                        (
                            "kotlin/ResultKt.createFailure".to_owned(),
                            true,
                            vec![Value::Cause]
                        ),
                        (
                            "kotlin/Result.constructor-impl".to_owned(),
                            true,
                            vec![Value::Call(0)]
                        ),
                    ],
                    Some(Value::Call(1)),
                    None,
                ),
            ]
        );
    }

    /// `let` is the degenerate body: it invokes the lambda with the receiver and returns that, with
    /// no calls of its own and no handler.
    #[test]
    fn let_invokes_with_the_receiver_and_returns_the_invocation() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        assert_eq!(
            simulated(
                stdlib,
                "kotlin/StandardKt__StandardKt",
                "let",
                "(Ljava/lang/Object;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
                &[0, 1],
                None,
            ),
            vec![(
                Vec::new(),
                Some(Value::Invocation),
                Some(SimulatedInvocation {
                    lambda_parameter: 1,
                    arguments: vec![Value::Parameter(0)],
                    after: 0,
                }),
            )]
        );
    }

    /// `use` is the shape the finally decode already handled: the cleanup runs on BOTH exits, reads
    /// the recorded cause, and the handler rethrows rather than producing a value. Seeding the cause
    /// local is what lets the normal exit name it the same way the handler does.
    #[test]
    fn use_names_its_recorded_cause_on_both_exits() {
        let Some(stdlib) = crate::toolchain::stdlib_jar() else {
            return;
        };
        let close_finally = |operands: Vec<Value>| {
            (
                "kotlin/io/CloseableKt.closeFinally".to_owned(),
                true,
                operands,
            )
        };
        assert_eq!(
            simulated(
                stdlib,
                "kotlin/io/CloseableKt",
                "use",
                "(Ljava/io/Closeable;Lkotlin/jvm/functions/Function1;)Ljava/lang/Object;",
                &[0, 1],
                Some(2),
            ),
            vec![
                (
                    vec![close_finally(vec![Value::Parameter(0), Value::Cause])],
                    Some(Value::Invocation),
                    Some(SimulatedInvocation {
                        lambda_parameter: 1,
                        arguments: vec![Value::Parameter(0)],
                        after: 0,
                    }),
                ),
                // The `Throwable` handler only records the cause and rethrows: no calls, no value.
                // Returning no value is what distinguishes a `finally` from an arm that recovers.
                (Vec::new(), None, None),
                // The catch-all handler carries the `finally` copy of the cleanup.
                (
                    vec![close_finally(vec![Value::Parameter(0), Value::Cause])],
                    None,
                    None,
                ),
            ]
        );
    }
}
