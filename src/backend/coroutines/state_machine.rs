//! The IR state machine a suspend function becomes on a target without stack switching.
//!
//! This is the shape Kotlin/Native and Kotlin/Wasm share: the function's frame lives in a
//! continuation object, and its body moves into that object's `invokeSuspend`, which runs as a
//! `while (true) { try { when (label) { … } } catch … }` dispatch over numbered states. Every
//! parameter and local becomes a typed field of the continuation, so nothing has to be proven live
//! across a suspension and the lowering stays linear in the body's size. That trades runtime speed
//! for compile speed and simplicity, which is the trade these targets want.
//!
//! The body must already be normalized (see the facade): every suspension point is a statement, a
//! local's initializer, or an assignment's value, and conditions do not suspend. A suspension
//! splits the current state in two. Its call receives the continuation; when the callee returns
//! [`CoroutineAbi::suspended`] the machine returns it, and a later `invokeSuspend` resumes in the
//! RESUME state, which rethrows a failed result and binds the resumed value. When the callee
//! returns directly, the value is bound in place and control continues in the CONTINUE state.
//!
//! Exceptions are routed by state: a `try` whose body suspends records, for each state its body
//! spans, the handler state that catches there. The dispatch's own `catch` sends an exception
//! thrown in such a state to its handler, which type-tests it against the source `catch` clauses
//! and rethrows what none of them takes; anything thrown elsewhere leaves `invokeSuspend`.
//!
//! The machine is plain common IR: fields, `when`, `while`, `try`, `throw`. A target realizes the
//! continuation class, the suspension marker and result encoding through [`CoroutineAbi`].

use std::collections::{HashMap, HashSet};

use crate::ir::{
    for_each_child, ClassId, ExprId, IrBinOp, IrCatch, IrConst, IrExpr, IrFile, IrTypeOp,
};
use crate::types::Ty;

use super::bottom_completion::unwrap_suspend_cast;
use super::control_flow::stmt_diverges;
use super::suspension_points::{expr_calls_suspend, is_suspension_point};

/// The construct a lowering declined, phrased for a diagnostic.
pub(crate) type Unsupported = String;

/// How a target represents the pieces of the coroutine protocol the machine touches.
pub(crate) trait CoroutineAbi: super::CoroutineRepresentation {
    /// The value a suspend call returns to say it suspended (`COROUTINE_SUSPENDED`).
    fn suspended(&self, ir: &mut IrFile) -> ExprId;

    /// A statement that throws the exception a resumed result carries, if it carries one.
    fn throw_if_failure(&self, ir: &mut IrFile, result: ExprId) -> ExprId;

    /// The value of type `ty` a successful raw result (`Any?`) carries.
    fn resumed_value(&self, ir: &mut IrFile, raw: ExprId, ty: Ty) -> ExprId;

    /// `value` of type `ty` as the `Any?` a suspend function returns.
    fn returned_value(&self, ir: &mut IrFile, value: ExprId, ty: Ty) -> ExprId;

    /// The suspension point `point` with `continuation` passed to it. A call gains the trailing
    /// continuation argument; an intrinsic point already reads its continuation.
    fn pass_continuation(
        &self,
        ir: &mut IrFile,
        point: ExprId,
        continuation: ExprId,
    ) -> Result<ExprId, Unsupported>;

    /// The coroutine context of `continuation` (`coroutineContext` inside the body).
    fn context(&self, ir: &mut IrFile, continuation: ExprId) -> ExprId;

    /// A statement that fails on a label no state answers. Unreachable in a correct machine.
    fn unknown_state(&self, ir: &mut IrFile) -> ExprId;
}

/// Where the machine keeps its state on the continuation object.
pub(crate) struct MachineFrame {
    /// The continuation class whose `invokeSuspend` runs the machine.
    pub(crate) class: ClassId,
    /// The `label` field: the state to run next.
    pub(crate) label: u32,
    /// The field holding the exception a handler state is about to route.
    pub(crate) exception: u32,
    /// The field each parameter and local of the function lives in, by its value index.
    pub(crate) values: HashMap<u32, u32>,
    /// The declared type of each such value.
    pub(crate) value_types: HashMap<u32, Ty>,
}

/// What [`build`] needs to know about the function it splits.
pub(crate) struct MachineInput<'a> {
    pub(crate) body: ExprId,
    /// The function's declared (pre-CPS) result.
    pub(crate) declared_result: Ty,
    pub(crate) suspend_set: &'a HashSet<u32>,
    /// Every function's declared (pre-CPS) result, indexed by `FunId`.
    pub(crate) declared_results: &'a [Ty],
}

/// `invokeSuspend`'s own value indices: the continuation itself and the resumed result.
const THIS: u32 = 0;
const RESUMED: u32 = 1;
const DISPATCH: &str = "$coroutine$dispatch";

/// Build `invokeSuspend`'s body for the normalized function body `input.body`, whose values live in
/// `frame`. The body's nodes are rewritten in place; the function that owned them must not be
/// emitted with that body afterwards.
pub(crate) fn build_state_machine(
    ir: &mut IrFile,
    abi: &dyn CoroutineAbi,
    frame: &MachineFrame,
    input: &MachineInput<'_>,
) -> Result<ExprId, Unsupported> {
    let mut machine = Machine {
        ir,
        abi,
        frame,
        input,
        states: vec![Vec::new()],
        handlers: vec![None],
        current: 0,
        handler: None,
        loops: Vec::new(),
        next_value: RESUMED + 1,
    };
    let statements = match machine.ir.exprs[input.body as usize].clone() {
        IrExpr::Block { stmts, value: None } => stmts,
        IrExpr::Block { value: Some(_), .. } => {
            return Err("a suspend function body that ends in a value".to_string())
        }
        _ => vec![input.body],
    };
    machine.flatten_statements(&statements)?;
    if !machine.current_diverges() {
        let unit = machine.ir.add_expr(IrExpr::UnitInstance);
        let unit = machine.abi.returned_value(machine.ir, unit, Ty::Unit);
        machine.push(IrExpr::Return(Some(unit)));
    }
    Ok(machine.assemble())
}

struct LoopFrame {
    label: Option<String>,
    continue_state: usize,
    break_state: usize,
}

struct Machine<'a, 'b> {
    ir: &'a mut IrFile,
    abi: &'a dyn CoroutineAbi,
    frame: &'a MachineFrame,
    input: &'a MachineInput<'b>,
    /// Each state's statements, in creation order; state 0 is the entry.
    states: Vec<Vec<ExprId>>,
    /// The handler state covering each state, parallel to `states`.
    handlers: Vec<Option<usize>>,
    current: usize,
    /// The handler state covering states created now.
    handler: Option<usize>,
    /// The source loops split across states, innermost last.
    loops: Vec<LoopFrame>,
    /// The next free value index in `invokeSuspend`.
    next_value: u32,
}

impl Machine<'_, '_> {
    fn new_state(&mut self) -> usize {
        self.states.push(Vec::new());
        self.handlers.push(self.handler);
        self.states.len() - 1
    }

    fn push(&mut self, expression: IrExpr) {
        let id = self.ir.add_expr(expression);
        self.states[self.current].push(id);
    }

    fn push_id(&mut self, id: ExprId) {
        self.states[self.current].push(id);
    }

    fn current_diverges(&self) -> bool {
        self.states[self.current]
            .last()
            .is_some_and(|&last| stmt_diverges(self.ir, last) || self.is_transfer(last))
    }

    /// Whether `statement` is a `label = …; continue` transfer this machine emitted.
    fn is_transfer(&self, statement: ExprId) -> bool {
        matches!(&self.ir.exprs[statement as usize],
            IrExpr::Block { stmts, value: None }
                if stmts.last().is_some_and(|&last| matches!(&self.ir.exprs[last as usize],
                    IrExpr::Continue { label: Some(label) } if label == DISPATCH)))
    }

    fn this(&mut self) -> ExprId {
        self.ir.add_expr(IrExpr::GetValue(THIS))
    }

    fn get_field(&mut self, index: u32) -> ExprId {
        let receiver = self.this();
        self.ir.add_expr(IrExpr::GetField {
            receiver,
            class: self.frame.class,
            index,
        })
    }

    fn set_field(&mut self, index: u32, value: ExprId) -> ExprId {
        let receiver = self.this();
        self.ir.add_expr(IrExpr::SetField {
            receiver,
            class: self.frame.class,
            index,
            value,
        })
    }

    /// `label = target; continue <dispatch>`, as one statement.
    fn transfer(&mut self, target: usize) -> ExprId {
        let state = self.ir.add_expr(IrExpr::Const(IrConst::Int(target as i32)));
        let set = self.set_field(self.frame.label, state);
        let jump = self.ir.add_expr(IrExpr::Continue {
            label: Some(DISPATCH.to_string()),
        });
        self.ir.add_expr(IrExpr::Block {
            stmts: vec![set, jump],
            value: None,
        })
    }

    fn goto(&mut self, target: usize) {
        let transfer = self.transfer(target);
        self.push_id(transfer);
    }

    fn fresh_value(&mut self) -> u32 {
        let value = self.next_value;
        self.next_value += 1;
        value
    }

    fn flatten_statements(&mut self, statements: &[ExprId]) -> Result<(), Unsupported> {
        for &statement in statements {
            if self.current_diverges() {
                // Code after a transfer or a `return` never runs; a state ends at its first exit.
                break;
            }
            self.flatten_statement(statement)?;
        }
        Ok(())
    }

    fn splits(&self, statement: ExprId) -> bool {
        expr_calls_suspend(self.ir, statement, self.input.suspend_set)
    }

    fn flatten_statement(&mut self, statement: ExprId) -> Result<(), Unsupported> {
        if !self.splits(statement) {
            let rewritten = self.localize(statement, &mut Vec::new())?;
            self.push_id(rewritten);
            return Ok(());
        }
        match self.ir.exprs[statement as usize].clone() {
            IrExpr::Block { stmts, value: None } => self.flatten_statements(&stmts),
            IrExpr::Variable {
                index,
                init: Some(init),
                ..
            } if self.is_point(init) => self.suspend(init, Some(index)),
            IrExpr::SetValue { var, value } if self.is_point(value) => {
                self.suspend(value, Some(var))
            }
            IrExpr::Return(Some(value)) if self.is_point(value) => self.suspend_return(value),
            IrExpr::When { branches } => self.flatten_when(branches),
            IrExpr::While {
                cond,
                body,
                update,
                post_test,
                label,
            } => self.flatten_loop(cond, body, update, post_test, label),
            IrExpr::Try {
                body,
                catches,
                finally: None,
                ..
            } => self.flatten_try(body, catches),
            IrExpr::Try {
                finally: Some(_), ..
            } => Err("a suspension inside a `try` with a `finally`".to_string()),
            _ if self.is_point(statement) => self.suspend(statement, None),
            _ => Err(format!(
                "a suspension in a `{}` statement the state machine does not split",
                self.suspension_path(statement)
            )),
        }
    }

    /// Whether `expression` is a suspension point, seen through the checked result coercion a
    /// generic suspend call carries.
    fn is_point(&self, expression: ExprId) -> bool {
        let point = unwrap_suspend_cast(self.ir, expression, self.input.suspend_set, false).point;
        is_suspension_point(self.ir, point, self.input.suspend_set)
    }

    /// The declared result of the suspension point `point`.
    fn point_result(&self, point: ExprId) -> Result<Ty, Unsupported> {
        super::suspension_points::suspend_call_fid(self.ir, point, self.input.suspend_set)
            .and_then(|function| self.input.declared_results.get(function as usize).copied())
            .or_else(|| super::suspension_points::recorded_suspension_result(self.ir, point))
            .ok_or_else(|| format!("a suspension point `{point}` with no recorded result type"))
    }

    /// Split at the suspension `expression`, binding its value to `target` when there is one.
    fn suspend(&mut self, expression: ExprId, target: Option<u32>) -> Result<(), Unsupported> {
        let point = unwrap_suspend_cast(self.ir, expression, self.input.suspend_set, false).point;
        let result_ty = self.point_result(point)?;
        let resume = self.new_state();
        let after = self.new_state();
        // The operands evaluate in the current state, against the frame's fields.
        let point = self.localize(point, &mut Vec::new())?;
        let continuation = self.this();
        let call = self.abi.pass_continuation(self.ir, point, continuation)?;
        let resume_label = self.ir.add_expr(IrExpr::Const(IrConst::Int(resume as i32)));
        let set_label = self.set_field(self.frame.label, resume_label);
        self.push_id(set_label);
        let raw = self.fresh_value();
        self.push(IrExpr::Variable {
            index: raw,
            ty: Ty::nullable(Ty::obj("kotlin/Any")),
            init: Some(call),
            named: false,
        });
        // `if (raw === COROUTINE_SUSPENDED) return raw`
        let read = self.ir.add_expr(IrExpr::GetValue(raw));
        let marker = self.abi.suspended(self.ir);
        let suspended = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::RefEq,
            lhs: read,
            rhs: marker,
        });
        let read = self.ir.add_expr(IrExpr::GetValue(raw));
        let leave = self.ir.add_expr(IrExpr::Return(Some(read)));
        self.push(IrExpr::When {
            branches: vec![(Some(suspended), leave)],
        });
        let read = self.ir.add_expr(IrExpr::GetValue(raw));
        self.bind_resumed(target, read, result_ty);
        self.goto(after);

        // Resumed by `invokeSuspend`: the result is in its parameter, possibly a failure.
        self.current = resume;
        let resumed = self.ir.add_expr(IrExpr::GetValue(RESUMED));
        let rethrow = self.abi.throw_if_failure(self.ir, resumed);
        self.push_id(rethrow);
        let resumed = self.ir.add_expr(IrExpr::GetValue(RESUMED));
        self.bind_resumed(target, resumed, result_ty);
        self.goto(after);

        self.current = after;
        Ok(())
    }

    /// `return <suspension>`: a tail call. Whatever the callee answers, `COROUTINE_SUSPENDED`
    /// included, is this function's answer, and a later resumption returns the resumed value.
    fn suspend_return(&mut self, expression: ExprId) -> Result<(), Unsupported> {
        let point = unwrap_suspend_cast(self.ir, expression, self.input.suspend_set, false).point;
        let resume = self.new_state();
        let point = self.localize(point, &mut Vec::new())?;
        let continuation = self.this();
        let call = self.abi.pass_continuation(self.ir, point, continuation)?;
        let resume_label = self.ir.add_expr(IrExpr::Const(IrConst::Int(resume as i32)));
        let set_label = self.set_field(self.frame.label, resume_label);
        self.push_id(set_label);
        self.push(IrExpr::Return(Some(call)));

        self.current = resume;
        let resumed = self.ir.add_expr(IrExpr::GetValue(RESUMED));
        let rethrow = self.abi.throw_if_failure(self.ir, resumed);
        self.push_id(rethrow);
        let resumed = self.ir.add_expr(IrExpr::GetValue(RESUMED));
        self.push(IrExpr::Return(Some(resumed)));
        Ok(())
    }

    fn bind_resumed(&mut self, target: Option<u32>, raw: ExprId, ty: Ty) {
        let Some(target) = target.filter(|&target| !self.is_unit_value(target)) else {
            return;
        };
        let declared = self.frame.value_types.get(&target).copied().unwrap_or(ty);
        let value = self.abi.resumed_value(self.ir, raw, declared);
        let field = self.frame.values[&target];
        let store = self.set_field(field, value);
        self.push_id(store);
    }

    fn flatten_when(&mut self, branches: Vec<(Option<ExprId>, ExprId)>) -> Result<(), Unsupported> {
        let join = self.new_state();
        let mut dispatch = Vec::with_capacity(branches.len() + 1);
        let mut bodies = Vec::with_capacity(branches.len());
        let mut exhaustive = false;
        for (condition, body) in branches {
            let condition = match condition {
                Some(condition) if self.splits(condition) => {
                    return Err("a suspension in a `when` condition".to_string())
                }
                Some(condition) => Some(self.localize(condition, &mut Vec::new())?),
                None => {
                    exhaustive = true;
                    None
                }
            };
            let state = self.new_state();
            let transfer = self.transfer(state);
            dispatch.push((condition, transfer));
            bodies.push((state, body));
        }
        if !exhaustive {
            let transfer = self.transfer(join);
            dispatch.push((None, transfer));
        }
        self.push(IrExpr::When { branches: dispatch });
        for (state, body) in bodies {
            self.current = state;
            self.flatten_statement(body)?;
            if !self.current_diverges() {
                self.goto(join);
            }
        }
        self.current = join;
        Ok(())
    }

    fn flatten_loop(
        &mut self,
        condition: ExprId,
        body: ExprId,
        update: Option<ExprId>,
        post_test: bool,
        label: Option<String>,
    ) -> Result<(), Unsupported> {
        if self.splits(condition) || update.is_some_and(|update| self.splits(update)) {
            return Err("a suspension in a loop condition".to_string());
        }
        let head = self.new_state();
        let entry = self.new_state();
        let step = update.map(|_| self.new_state());
        let exit = self.new_state();
        self.goto(if post_test { entry } else { head });

        self.current = head;
        let condition = self.localize(condition, &mut Vec::new())?;
        let enter = self.transfer(entry);
        let leave = self.transfer(exit);
        self.push(IrExpr::When {
            branches: vec![(Some(condition), enter), (None, leave)],
        });

        self.current = entry;
        let next = step.unwrap_or(head);
        self.loops.push(LoopFrame {
            label,
            continue_state: next,
            break_state: exit,
        });
        self.flatten_statement(body)?;
        if !self.current_diverges() {
            self.goto(next);
        }

        // Still inside the loop: a counted loop's update leaves it by `break` at its last value.
        if let (Some(step), Some(update)) = (step, update) {
            self.current = step;
            let update = self.localize(update, &mut Vec::new())?;
            self.push_id(update);
            if !self.current_diverges() {
                self.goto(head);
            }
        }
        self.loops.pop();
        self.current = exit;
        Ok(())
    }

    fn flatten_try(&mut self, body: ExprId, catches: Vec<IrCatch>) -> Result<(), Unsupported> {
        let outer = self.handler;
        let handler = self.new_state();
        let after = self.new_state();
        self.handler = Some(handler);
        let protected = self.new_state();
        self.handler = outer;
        self.goto(protected);

        self.current = protected;
        self.handler = Some(handler);
        self.flatten_statement(body)?;
        self.handler = outer;
        if !self.current_diverges() {
            self.goto(after);
        }

        // The handler runs under the enclosing handler: a clause that takes nothing rethrows there.
        self.current = handler;
        let mut clauses = Vec::with_capacity(catches.len() + 1);
        let mut bodies = Vec::with_capacity(catches.len());
        for catch in catches {
            let state = self.new_state();
            let caught = self.get_field(self.frame.exception);
            let test = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::InstanceOf,
                arg: caught,
                type_operand: catch.ty,
            });
            let caught = self.get_field(self.frame.exception);
            let typed = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: caught,
                type_operand: catch.ty,
            });
            let field = *self
                .frame
                .values
                .get(&catch.var)
                .ok_or_else(|| "a `catch` whose exception has no frame field".to_string())?;
            let bind = self.set_field(field, typed);
            let transfer = self.transfer(state);
            let clause = self.ir.add_expr(IrExpr::Block {
                stmts: vec![bind, transfer],
                value: None,
            });
            clauses.push((Some(test), clause));
            bodies.push((state, catch.body));
        }
        let caught = self.get_field(self.frame.exception);
        let rethrow = self.ir.add_expr(IrExpr::Throw { operand: caught });
        clauses.push((None, rethrow));
        self.push(IrExpr::When { branches: clauses });
        for (state, body) in bodies {
            self.current = state;
            self.flatten_statement(body)?;
            if !self.current_diverges() {
                self.goto(after);
            }
        }
        self.current = after;
        Ok(())
    }

    /// Rewrite a statement that does not split to run against the frame: each parameter and local
    /// becomes its field, a `return` yields the function's `Any?` result, and a `break`/`continue`
    /// out of a split loop becomes a transfer. `intact` holds the labels of the unsplit loops the
    /// walk is inside, innermost last; an unlabeled jump belongs to the innermost of them.
    fn localize(
        &mut self,
        expression: ExprId,
        intact: &mut Vec<Option<String>>,
    ) -> Result<ExprId, Unsupported> {
        match self.ir.exprs[expression as usize].clone() {
            IrExpr::GetValue(index) if self.is_unit_value(index) => {
                self.ir.exprs[expression as usize] = IrExpr::UnitInstance;
            }
            IrExpr::SetValue { var, value } if self.is_unit_value(var) => {
                // A `Unit` has no state to keep: the value runs for its effect.
                self.localize(value, intact)?;
                self.ir.exprs[expression as usize] = self.ir.exprs[value as usize].clone();
            }
            IrExpr::Variable { index, init, .. } if self.is_unit_value(index) => match init {
                Some(init) => {
                    self.localize(init, intact)?;
                    self.ir.exprs[expression as usize] = self.ir.exprs[init as usize].clone();
                }
                None => self.ir.exprs[expression as usize] = IrExpr::UnitInstance,
            },
            IrExpr::GetValue(index) => {
                let field = self.value_field(index)?;
                let receiver = self.this();
                self.ir.exprs[expression as usize] = IrExpr::GetField {
                    receiver,
                    class: self.frame.class,
                    index: field,
                };
            }
            IrExpr::SetValue { var, value } => {
                let value = self.localize(value, intact)?;
                let field = self.value_field(var)?;
                let receiver = self.this();
                self.ir.exprs[expression as usize] = IrExpr::SetField {
                    receiver,
                    class: self.frame.class,
                    index: field,
                    value,
                };
            }
            IrExpr::Variable {
                index, ty, init, ..
            } => {
                let value = match init {
                    Some(init) => self.localize(init, intact)?,
                    None => {
                        let zero = self.abi.zero(&ty);
                        self.ir.add_expr(IrExpr::Const(zero))
                    }
                };
                let field = self.value_field(index)?;
                let receiver = self.this();
                self.ir.exprs[expression as usize] = IrExpr::SetField {
                    receiver,
                    class: self.frame.class,
                    index: field,
                    value,
                };
            }
            IrExpr::Return(value) => {
                let value = match value {
                    Some(value) => {
                        let value = self.localize(value, intact)?;
                        self.abi
                            .returned_value(self.ir, value, self.input.declared_result)
                    }
                    None => {
                        let unit = self.ir.add_expr(IrExpr::UnitInstance);
                        self.abi.returned_value(self.ir, unit, Ty::Unit)
                    }
                };
                self.ir.exprs[expression as usize] = IrExpr::Return(Some(value));
            }
            IrExpr::Break { label } | IrExpr::Continue { label }
                if !jumps_within(intact, label.as_deref()) =>
            {
                let breaking = matches!(self.ir.exprs[expression as usize], IrExpr::Break { .. });
                let frame = self
                    .loops
                    .iter()
                    .rev()
                    .find(|frame| label.is_none() || frame.label == label)
                    .ok_or_else(|| "a jump to a loop outside the suspend function".to_string())?;
                let target = if breaking {
                    frame.break_state
                } else {
                    frame.continue_state
                };
                let transfer = self.transfer(target);
                self.ir.exprs[expression as usize] = self.ir.exprs[transfer as usize].clone();
            }
            IrExpr::While {
                cond,
                body,
                update,
                label,
                ..
            } => {
                intact.push(label);
                self.localize(cond, intact)?;
                self.localize(body, intact)?;
                if let Some(update) = update {
                    self.localize(update, intact)?;
                }
                intact.pop();
            }
            IrExpr::Try {
                body,
                catches,
                finally,
                result,
            } => {
                self.localize(body, intact)?;
                let mut rebound = Vec::with_capacity(catches.len());
                for mut catch in catches {
                    // A clause binds its exception to an `invokeSuspend` local, then stores it in
                    // the frame field every read of it now names.
                    let field = self.value_field(catch.var)?;
                    let local = self.fresh_value();
                    catch.var = local;
                    let body = self.localize(catch.body, intact)?;
                    let caught = self.ir.add_expr(IrExpr::GetValue(local));
                    let store = self.set_field(field, caught);
                    catch.body = self.ir.add_expr(IrExpr::Block {
                        stmts: vec![store, body],
                        value: None,
                    });
                    rebound.push(catch);
                }
                if let Some(finally) = finally {
                    self.localize(finally, intact)?;
                }
                self.ir.exprs[expression as usize] = IrExpr::Try {
                    body,
                    catches: rebound,
                    finally,
                    result,
                };
            }
            IrExpr::CurrentContinuation => {
                self.ir.exprs[expression as usize] = IrExpr::GetValue(THIS);
            }
            IrExpr::Call {
                callee:
                    crate::ir::Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::CoroutineContext,
                        ..
                    },
                ..
            } => {
                let continuation = self.this();
                let context = self.abi.context(self.ir, continuation);
                self.ir.exprs[expression as usize] = self.ir.exprs[context as usize].clone();
            }
            IrExpr::Lambda { captures, .. } => {
                // The body is its own function; only the captured values are read here.
                for capture in captures {
                    self.localize(capture, intact)?;
                }
            }
            _ => {
                let mut children = Vec::new();
                for_each_child(&self.ir.exprs, expression, &mut |child| {
                    children.push(child)
                });
                for child in children {
                    self.localize(child, intact)?;
                }
            }
        }
        Ok(expression)
    }

    /// Whether `index` is a `Unit` value, which the frame keeps no field for: every read of it
    /// is the `Unit` object.
    fn is_unit_value(&self, index: u32) -> bool {
        !self.frame.values.contains_key(&index)
            && self.frame.value_types.get(&index) == Some(&Ty::Unit)
    }

    fn value_field(&self, index: u32) -> Result<u32, Unsupported> {
        self.frame
            .values
            .get(&index)
            .copied()
            .ok_or_else(|| "a suspend function value with no frame field".to_string())
    }

    /// `invokeSuspend`'s body: the dispatch loop over every state, with exception routing.
    fn assemble(mut self) -> ExprId {
        let mut arms = Vec::with_capacity(self.states.len() + 1);
        let states = std::mem::take(&mut self.states);
        for (index, statements) in states.into_iter().enumerate() {
            let label = self.get_field(self.frame.label);
            let state = self.ir.add_expr(IrExpr::Const(IrConst::Int(index as i32)));
            let test = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: label,
                rhs: state,
            });
            let body = self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: None,
            });
            arms.push((Some(test), body));
        }
        let unknown = self.abi.unknown_state(self.ir);
        arms.push((None, unknown));
        let dispatch = self.ir.add_expr(IrExpr::When { branches: arms });

        // Route an exception by the state it was thrown in.
        let caught = self.fresh_value();
        let mut routes = Vec::new();
        let handlers = std::mem::take(&mut self.handlers);
        for (handler, states) in group_by_handler(&handlers) {
            let mut test = None;
            for state in states {
                let label = self.get_field(self.frame.label);
                let value = self.ir.add_expr(IrExpr::Const(IrConst::Int(state as i32)));
                let equal = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: label,
                    rhs: value,
                });
                test = Some(match test {
                    None => equal,
                    Some(previous) => self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Or,
                        lhs: previous,
                        rhs: equal,
                    }),
                });
            }
            let exception = self.ir.add_expr(IrExpr::GetValue(caught));
            let store = self.set_field(self.frame.exception, exception);
            let transfer = self.transfer(handler);
            let route = self.ir.add_expr(IrExpr::Block {
                stmts: vec![store, transfer],
                value: None,
            });
            routes.push((test, route));
        }
        let body = if routes.is_empty() {
            dispatch
        } else {
            let exception = self.ir.add_expr(IrExpr::GetValue(caught));
            let rethrow = self.ir.add_expr(IrExpr::Throw { operand: exception });
            routes.push((None, rethrow));
            let route = self.ir.add_expr(IrExpr::When { branches: routes });
            self.ir.add_expr(IrExpr::Try {
                body: dispatch,
                catches: vec![IrCatch {
                    var: caught,
                    binding: None,
                    ty: Ty::obj("kotlin/Throwable"),
                    body: route,
                    line: None,
                }],
                finally: None,
                result: Ty::Unit,
            })
        };
        let forever = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let dispatch_loop = self.ir.add_expr(IrExpr::While {
            cond: forever,
            body,
            update: None,
            post_test: false,
            label: Some(DISPATCH.to_string()),
        });
        self.ir.add_expr(IrExpr::Block {
            stmts: vec![dispatch_loop],
            value: None,
        })
    }
}

/// Whether a jump with `label` targets one of the unsplit loops it sits in.
fn jumps_within(intact: &[Option<String>], label: Option<&str>) -> bool {
    match label {
        None => !intact.is_empty(),
        Some(label) => intact
            .iter()
            .any(|loop_label| loop_label.as_deref() == Some(label)),
    }
}

/// The states each handler covers, by handler, in state order.
fn group_by_handler(handlers: &[Option<usize>]) -> Vec<(usize, Vec<usize>)> {
    let mut grouped: Vec<(usize, Vec<usize>)> = Vec::new();
    for (state, handler) in handlers.iter().enumerate() {
        let Some(handler) = *handler else {
            continue;
        };
        match grouped
            .iter_mut()
            .find(|(existing, _)| *existing == handler)
        {
            Some((_, states)) => states.push(state),
            None => grouped.push((handler, vec![state])),
        }
    }
    grouped
}

/// The variant an expression is, for a diagnostic.
impl Machine<'_, '_> {
    /// The node kinds from `statement` down to its first suspension, `RefSet > TypeOp > Call`.
    fn suspension_path(&self, statement: ExprId) -> String {
        let mut path = vec![variant_name(&self.ir.exprs[statement as usize])];
        let mut at = statement;
        while !self.is_point(at) {
            let mut next = None;
            crate::ir::for_each_child(&self.ir.exprs, at, &mut |child| {
                if next.is_none() && expr_calls_suspend(self.ir, child, self.input.suspend_set) {
                    next = Some(child);
                }
            });
            let Some(child) = next else { break };
            path.push(variant_name(&self.ir.exprs[child as usize]));
            at = child;
        }
        path.join(" > ")
    }
}

fn variant_name(expression: &IrExpr) -> String {
    format!("{expression:?}")
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}
