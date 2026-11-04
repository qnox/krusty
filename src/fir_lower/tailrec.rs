//! Tail-position rewriting for checked self calls.
//!
//! Calls reach this module only after FIR has selected a stable callable identity and mapped every
//! argument to its declaration parameter. The transform therefore contains no name lookup or
//! overload logic.

use crate::fir::{OriginId, SyntheticOriginKind};
use crate::ir::{Callee, ExprId, FunId, IrConst, IrExpr, IrFile, IrNodeOrigin};
use crate::types::Ty;
use std::rc::Rc;

use super::FirLoweringFailure;

const LOOP_LABEL: &str = "$tailrec";

/// kotlinc's `TailrecLowering`: the body runs inside `do { <body>; break } while (true)`, and each
/// self-call in tail position becomes a loop step — the next turn's parameters written, then
/// `continue`. The body keeps its own returns: an expression body returns its value, and a `Unit`
/// body leaves the loop through the `break` and returns after it. A body with no tail call is left
/// as an ordinary body, as kotlinc leaves it.
///
/// The step's own statements carry no source line of their own, as kotlinc's are built at the
/// call.
pub(super) fn finish_tailrec_body(
    ir: &mut IrFile,
    mut roots: Vec<ExprId>,
    mut frame: Frame,
    implicit_return: bool,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    let result = ir.functions[frame.function as usize].ret;
    let unit = result == Ty::Unit;
    frame.survey(ir, &roots);
    let frame = &frame;
    if implicit_return {
        if unit {
            super::consume_trailing_unit_result(ir, &mut roots);
        } else {
            let value = roots
                .pop()
                .ok_or(FirLoweringFailure::MissingBodyResult { origin })?;
            let returned = generated(ir, IrExpr::Return(Some(value)), origin);
            if let Some(&end) = ir.expr_end_lines.get(&value) {
                ir.mark_implicit_return_end_line(returned, end);
            }
            roots.push(returned);
        }
    }
    let tail_calls = loop {
        let walk = tail_calls(ir, &roots, frame, unit);
        match walk.uncaptured.first() {
            Some(lambda) => capture_parameters(ir, lambda),
            None => break walk.found,
        }
    };
    if tail_calls.is_empty() {
        return super::finish_callable_body(
            ir,
            roots,
            result,
            unit && implicit_return,
            false,
            Some(super::UnitReturnLine::ExpressionEnd),
            origin,
        );
    }
    for TailCall {
        edge,
        call,
        line,
        frame: lambda_frame,
    } in tail_calls
    {
        let frame = lambda_frame.as_deref().unwrap_or(frame);
        let step = loop_step(ir, call, line, frame, origin);
        let step = generated(ir, step, origin);
        edge.replace(ir, &mut roots, step);
    }
    // kotlinc builds the `break` at the body's own position, so it carries the line the body
    // starts on.
    let exit = generated(
        ir,
        IrExpr::Break {
            label: Some(LOOP_LABEL.to_owned()),
        },
        origin,
    );
    if let Some(&line) = ir.fn_decl_lines.get(&frame.function) {
        ir.expr_lines.insert(exit, line);
    }
    roots.push(exit);
    let loop_body = generated(
        ir,
        IrExpr::Block {
            stmts: roots,
            value: None,
        },
        origin,
    );
    let condition = generated(ir, IrExpr::Const(IrConst::Boolean(true)), origin);
    let loop_expression = generated(
        ir,
        IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            // kotlinc's `do … while (true)`: a step's `continue` goes to the condition, which
            // jumps back to the top.
            post_test: true,
            label: Some(LOOP_LABEL.to_owned()),
        },
        origin,
    );
    // A `Unit` function returns after the loop through the method's own implicit `return`.
    Ok(generated(
        ir,
        IrExpr::Block {
            stmts: vec![loop_expression],
            value: None,
        },
        origin,
    ))
}

/// A self-call in tail position: the slot holding it, and the source line it is on (its own, or
/// the nearest enclosing node's when the lowering gave it none).
struct TailCall {
    edge: Edge,
    call: ExprId,
    line: Option<u32>,
    /// The frame of the inline lambda body the call is written in; `None` in the function's own
    /// body.
    frame: Option<Rc<Frame>>,
}

/// Where a tail position sits: the one child slot a loop step replaces.
#[derive(Clone, Copy, Debug)]
enum Edge {
    Root(usize),
    Statement(ExprId, usize),
    BlockValue(ExprId),
    Branch(ExprId, usize),
    /// A `return` no replaceable slot holds — one nested in an operand. The step takes the
    /// `return`'s own node: it leaves the turn exactly where the `return` left the function.
    Return(ExprId),
}

impl Edge {
    fn replace(self, ir: &mut IrFile, roots: &mut [ExprId], step: ExprId) {
        match self {
            Edge::Root(index) => roots[index] = step,
            Edge::Statement(block, index) => {
                let IrExpr::Block { stmts, .. } = &mut ir.exprs[block as usize] else {
                    unreachable!("a statement edge names a block")
                };
                stmts[index] = step;
            }
            Edge::BlockValue(block) => {
                let IrExpr::Block { value, .. } = &mut ir.exprs[block as usize] else {
                    unreachable!("a block-value edge names a block")
                };
                *value = Some(step);
            }
            Edge::Branch(when, index) => {
                let IrExpr::When { branches } = &mut ir.exprs[when as usize] else {
                    unreachable!("a branch edge names a when")
                };
                branches[index].1 = step;
            }
            Edge::Return(returned) => {
                ir.exprs[returned as usize] = ir.exprs[step as usize].clone();
                if let Some(origin) = ir.fir_origins.get(&step).cloned() {
                    ir.fir_origins.insert(returned, origin);
                }
                ir.checked_return_depths.remove(&returned);
            }
        }
    }
}

/// kotlinc's `collectTailRecursionCalls`: the self-calls in tail position, each with the edge that
/// holds it.
///
/// A position is a tail when nothing of this function runs after it: the value of a `return` of
/// this function, the last statement of a tail block, a branch result of a tail `when`, the operand
/// of a tail coercion, and in a `Unit` function any statement a `return` follows. A `try` is never
/// entered — its `finally` runs after the value. An inline lambda's body is entered in its own
/// frame: only a `return` of this function written in it (a non-local return) is a tail position
/// there, and its step leaves the lambda for the loop as a non-local `continue` (kotlinc's
/// `TailRecursionCallsCollector` visits an inline lambda's returns of the function the same way).
/// A `return` or coercion that holds the call directly is replaced along with it: the step leaves
/// the loop turn and produces no value for either.
///
/// Only a node reached by exactly ONE path is rewritten. A call below a shared ancestor is seen by
/// another path too, which may cross a boundary this walk does not enter, so it stays a call.
fn tail_calls(ir: &IrFile, roots: &[ExprId], frame: &Frame, unit: bool) -> TailCalls {
    struct Walk<'a> {
        ir: &'a IrFile,
        frame: &'a Frame,
        /// `frame` when it is an inline lambda body's, for the tail calls found in it.
        lambda_frame: Option<Rc<Frame>>,
        /// Whether the walk is inside an inline lambda's body.
        in_inline_body: bool,
        unit: bool,
        paths: &'a crate::fx_hash::HashMap<ExprId, u8>,
        found: Vec<TailCall>,
        /// The outermost inline lambda around this walk whose frame lacks a parameter of the
        /// frame it is written in.
        uncaptured_lambda: Option<Rc<UncapturedParameters>>,
        uncaptured: Vec<Rc<UncapturedParameters>>,
        seen: crate::fx_hash::HashSet<ExprId>,
        /// The source line of the innermost node visited that has one.
        line: Option<u32>,
    }

    impl Walk<'_> {
        /// Visit `expression`, a tail position when `tail`, held by `edge` when the slot holding it
        /// can be replaced.
        fn visit(&mut self, expression: ExprId, tail: bool, edge: Option<Edge>) {
            if !self.seen.insert(expression) {
                return;
            }
            let outer = self.line;
            if let Some(&line) = self
                .ir
                .expr_source_lines
                .get(&expression)
                .or_else(|| self.ir.expr_lines.get(&expression))
            {
                self.line = Some(line);
            }
            self.visit_node(expression, tail, edge);
            self.line = outer;
        }

        fn visit_node(&mut self, expression: ExprId, tail: bool, edge: Option<Edge>) {
            match self.ir.expr(expression) {
                IrExpr::Try { .. } => {}
                IrExpr::Lambda {
                    captures,
                    impl_fn,
                    inline_body,
                    ..
                } => {
                    let (captures, impl_fn, inline_body) =
                        (captures.clone(), *impl_fn, *inline_body);
                    for &capture in &captures {
                        self.visit(capture, false, None);
                    }
                    if let Some(body) = inline_body {
                        self.visit_inline_body(expression, body, &captures, impl_fn);
                    }
                }
                IrExpr::Call { .. } | IrExpr::MethodCall { .. } => {
                    self.children(expression);
                    if let Some(edge) = edge.filter(|_| {
                        tail && self.is_self_call(expression) && !self.step_suspends(expression)
                    }) {
                        match &self.uncaptured_lambda {
                            Some(lambda) => self.uncaptured.push(lambda.clone()),
                            None => self.found.push(TailCall {
                                edge,
                                call: expression,
                                line: self.line,
                                frame: self.lambda_frame.clone(),
                            }),
                        }
                    }
                }
                IrExpr::Return(Some(value)) => {
                    let value = *value;
                    let holder = edge.or(Some(Edge::Return(expression)));
                    self.visit_held(value, self.returns_from_function(expression), holder);
                }
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => {
                    let arg = *arg;
                    self.visit_held(arg, tail, edge);
                }
                // A PLATFORM-NARROWING not-null assertion (`NullCheck::Named`) wraps the value without
                // consuming the tail position: a stepped call yields nothing to assert, and
                // kotlinc likewise drops the check when it loops the call. An explicit source
                // `!!` (`NullCheck::Source`) is neither — kotlinc reports the call under it as not a
                // tail call and runs the check on every frame, so it stays opaque here.
                IrExpr::NotNullAssert {
                    operand,
                    check: crate::ir::NullCheck::Named(_),
                } => {
                    let operand = *operand;
                    self.visit_held(operand, tail, edge);
                }
                IrExpr::Block { stmts, value } => {
                    let (stmts, value) = (stmts.clone(), *value);
                    let coerced_to_unit = self.unit
                        && value.is_some_and(|value| {
                            matches!(self.ir.expr(value), IrExpr::UnitInstance)
                        });
                    for (index, &statement) in stmts.iter().enumerate() {
                        let statement_tail = match stmts.get(index + 1) {
                            Some(&next) => self.unit && self.returns_unit(next),
                            None => tail && (value.is_none() || coerced_to_unit),
                        };
                        self.visit(
                            statement,
                            statement_tail,
                            Some(Edge::Statement(expression, index)),
                        );
                    }
                    if let Some(value) = value {
                        self.visit(value, tail, Some(Edge::BlockValue(expression)));
                    }
                }
                IrExpr::When { branches } => {
                    for (index, (condition, result)) in branches.clone().into_iter().enumerate() {
                        if let Some(condition) = condition {
                            self.visit(condition, false, None);
                        }
                        self.visit(result, tail, Some(Edge::Branch(expression, index)));
                    }
                }
                _ => self.children(expression),
            }
        }

        /// The operand of a `return` or a coercion. The holder's edge travels down through
        /// transparent coercions, so a self-call reached through them takes the holder's place:
        /// the step leaves the turn and yields nothing to return or convert. A block or `when`
        /// replaces the edge with its own child edges; any other node drops it.
        fn visit_held(&mut self, held: ExprId, tail: bool, holder: Option<Edge>) {
            self.visit(held, tail, holder);
        }

        /// The body of an inline lambda written in this frame, walked in the lambda's own frame.
        /// Nothing in it runs as this frame's tail but the returns of the function it holds.
        fn visit_inline_body(
            &mut self,
            lambda: ExprId,
            body: ExprId,
            captures: &[ExprId],
            impl_fn: FunId,
        ) {
            let frame = Rc::new(Frame::of_inline_body(
                self.ir, self.frame, captures, impl_fn, body,
            ));
            let uncaptured_lambda = self.uncaptured_lambda.clone().or_else(|| {
                let parameters = frame.uncaptured_parameters(self.frame);
                (!parameters.is_empty())
                    .then(|| Rc::new(UncapturedParameters { lambda, parameters }))
            });
            let mut walk = Walk {
                ir: self.ir,
                frame: &frame,
                lambda_frame: Some(frame.clone()),
                in_inline_body: true,
                unit: self.unit,
                paths: self.paths,
                found: Vec::new(),
                uncaptured_lambda,
                uncaptured: Vec::new(),
                seen: crate::fx_hash::HashSet::default(),
                line: self.line,
            };
            walk.visit(body, false, None);
            self.found.append(&mut walk.found);
            self.uncaptured.append(&mut walk.uncaptured);
        }

        /// Whether `returned` leaves the function, not an inline lambda the walk is inside.
        fn returns_from_function(&self, returned: ExprId) -> bool {
            if self.in_inline_body {
                // The lambda's template was prepared for the boundary it crosses into this body
                // (`inline_returns::prepare_inline_template`): its own returns became exits of its
                // frame, and a return of the function moved to depth zero. A `return` lowering
                // generated there carries no depth and is the lambda's own.
                self.ir.checked_return_depths.get(&returned) == Some(&0)
            } else {
                returns_from_here(self.ir, returned)
            }
        }

        /// A self-call in a frame that lacks a parameter is one only once the lambda captures it:
        /// the walk reports the lambda instead of the call.
        fn is_self_call(&self, call: ExprId) -> bool {
            self.paths.get(&call) == Some(&1)
                && (self.uncaptured_lambda.is_some() || self.frame.holds_every_position())
                && is_self_call(self.ir, call, self.frame)
        }

        /// See [`stepped_evaluation_suspends`].
        fn step_suspends(&self, call: ExprId) -> bool {
            stepped_evaluation_suspends(self.ir, call, self.frame)
        }

        fn children(&mut self, expression: ExprId) {
            let mut children = Vec::new();
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| {
                children.push(child)
            });
            for child in children {
                self.visit(child, false, None);
            }
        }

        /// A `return` of this function, bare or of `Unit`.
        fn returns_unit(&self, statement: ExprId) -> bool {
            self.returns_from_function(statement)
                && match self.ir.expr(statement) {
                    IrExpr::Return(None) => true,
                    IrExpr::Return(Some(value)) => {
                        matches!(self.ir.expr(*value), IrExpr::UnitInstance)
                    }
                    _ => false,
                }
        }
    }

    let paths = root_path_counts(ir, roots);
    let mut walk = Walk {
        ir,
        frame,
        lambda_frame: None,
        in_inline_body: false,
        unit,
        paths: &paths,
        found: Vec::new(),
        uncaptured_lambda: None,
        uncaptured: Vec::new(),
        seen: crate::fx_hash::HashSet::default(),
        line: None,
    };
    for (index, &root) in roots.iter().enumerate() {
        let tail = match roots.get(index + 1) {
            Some(&next) => unit && walk.returns_unit(next),
            None => true,
        };
        walk.visit(root, tail, Some(Edge::Root(index)));
    }
    TailCalls {
        found: walk.found,
        uncaptured: walk.uncaptured,
    }
}

/// What [`tail_calls`] found: the self-calls in tail position a loop step replaces, and the inline
/// lambdas that must first capture a parameter a self-call written in them writes.
struct TailCalls {
    found: Vec<TailCall>,
    uncaptured: Vec<Rc<UncapturedParameters>>,
}

/// An inline lambda whose body holds a self-call in tail position but does not capture every
/// parameter of the frame it is written in. kotlinc's `TailrecLowering` runs before its lambdas
/// capture anything, so the step's write of such a parameter makes the lambda capture it; here the
/// lambda is already lowered, and [`capture_parameters`] gives it those captures.
struct UncapturedParameters {
    lambda: ExprId,
    /// The parameters it lacks, as the enclosing frame holds them.
    parameters: Vec<FrameParameter>,
}

/// A parameter as a frame holds it: its value slot there, and the function whose parameter list
/// that slot indexes (the function's own, or an inline lambda's, whose captures lead its list).
#[derive(Clone, Copy, Debug)]
struct FrameParameter {
    slot: u32,
    numbering: FunId,
    index: usize,
}

/// Give `uncaptured.lambda` a capture of each parameter it lacks, after its own captures. The
/// lambda's frame numbers its captures first, so its parameters and locals each move up one slot
/// per capture added, in both its method body and its inline template; the inliner binds the new
/// capture to the enclosing frame's own slot, so the step's write reaches it.
fn capture_parameters(ir: &mut IrFile, uncaptured: &UncapturedParameters) {
    for parameter in &uncaptured.parameters {
        let IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body,
            ..
        } = ir.expr(uncaptured.lambda).clone()
        else {
            unreachable!("an uncaptured parameter belongs to a lambda")
        };
        let position = u32::try_from(captures.len()).expect("capture count fits packed value ids");
        let index = captures.len();
        let ty = ir.functions[parameter.numbering as usize].params[parameter.index];
        let source_name = ir
            .fn_params
            .get(&parameter.numbering)
            .and_then(|info| info.identities.get(parameter.index))
            .and_then(|identity| identity.source_name.clone());
        let read = ir.add_expr(IrExpr::GetValue(parameter.slot));
        let bodies = [inline_body, ir.functions[impl_fn as usize].body];
        for body in bodies.into_iter().flatten() {
            crate::ir::shift_value_indices(ir, body, position, 1);
        }
        ir.resize_lambda_capture_prefix(impl_fn, index, 0, 1);
        let function = &mut ir.functions[impl_fn as usize];
        function.params.insert(index, ty);
        if function.param_checks.len() >= index && !function.param_checks.is_empty() {
            function.param_checks.insert(index, None);
        }
        if let Some(info) = ir.fn_params.get_mut(&impl_fn) {
            info.identities.insert(
                index,
                crate::ir::IrParameterIdentity::captured_value(
                    source_name,
                    position,
                    crate::ir::IrValueCapture {
                        declaration: crate::ir::IrCapturedDeclaration::Parameter,
                        capturer: crate::ir::IrCapturingCallable::Lambda,
                    },
                ),
            );
            if !info.inline_modifiers.is_empty() {
                info.inline_modifiers
                    .insert(index, crate::types::InlineParameterModifier::None);
            }
            if !info.expansion_modes.is_empty() {
                info.expansion_modes
                    .insert(index, crate::types::InlineExpansionMode::Materialize);
            }
        }
        let shared = ir
            .shared_capture_parameters
            .keys()
            .filter(|(function, slot)| *function == impl_fn && *slot >= position)
            .copied()
            .collect::<Vec<_>>();
        let moved = shared
            .into_iter()
            .filter_map(|key| {
                ir.shared_capture_parameters
                    .remove(&key)
                    .map(|ty| (key, ty))
            })
            .collect::<Vec<_>>();
        for ((function, slot), ty) in moved {
            ir.shared_capture_parameters
                .insert((function, slot + 1), ty);
        }
        let IrExpr::Lambda { captures, .. } = &mut ir.exprs[uncaptured.lambda as usize] else {
            unreachable!("an uncaptured parameter belongs to a lambda")
        };
        captures.push(read);
    }
}

/// Find every value slot that holds THIS frame's instance.
///
/// A member call does not read `this` at the call node. The lowering spills the receiver and each
/// argument into a generated temporary first, so the shape reaching the rewrite is
/// `{ t0 = this; t1 = n - 1; t2 = acc; this.f(t0, t1, t2) }` with the call reading `GetValue(t0)` —
/// evaluation order being the point of the spill. A receiver test that demanded the literal
/// receiver slot would therefore answer "different instance" for every member self-call.
///
/// So the aliases are followed: a generated, unnamed `Variable` initialized from a slot already
/// known to hold this instance holds it too, to a fixed point. Only `named: false` temporaries
/// qualify — a source `val` is the programmer's and the rewrite has no business assuming what stays
/// in it — and a slot the body ever reassigns is dropped, so an alias that is true at one point in
/// the body cannot be relied on at another.
///
/// Being conservative here costs nothing but a missed rewrite: a self-call this does not recognize
/// stays an ordinary call, which is what the program did before.
fn collect_this_slots(ir: &IrFile, roots: &[ExprId], frame: &mut Frame) {
    if !frame.member {
        return;
    }
    let mut bindings: Vec<(u32, ExprId)> = Vec::new();
    let mut assigned: crate::fx_hash::HashSet<u32> = crate::fx_hash::HashSet::default();
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::Variable {
                index,
                init: Some(init),
                named: false,
                ..
            } => bindings.push((*index, *init)),
            IrExpr::SetValue { var, .. } => {
                assigned.insert(*var);
            }
            _ => {}
        }
        for_each_frame_child(ir, expression, &mut |child| pending.push(child));
    }
    loop {
        let mut grew = false;
        for &(slot, init) in &bindings {
            if frame.this_slots.contains(&slot) || assigned.contains(&slot) {
                continue;
            }
            if matches!(ir.expr(init), IrExpr::GetValue(source) if frame.this_slots.contains(source))
            {
                frame.this_slots.insert(slot);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    frame.this_slots.retain(|slot| !assigned.contains(slot));
}

/// The compiler temporaries the body never reassigns — an argument the call lowering held to keep
/// source evaluation order among them. A read of one is already its own snapshot.
fn fixed_temporaries(ir: &IrFile, roots: &[ExprId]) -> crate::fx_hash::HashSet<u32> {
    let mut declared = crate::fx_hash::HashSet::default();
    let mut assigned = crate::fx_hash::HashSet::default();
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::Variable {
                index,
                named: false,
                ..
            } => {
                declared.insert(*index);
            }
            IrExpr::SetValue { var, .. } => {
                assigned.insert(*var);
            }
            _ => {}
        }
        for_each_frame_child(ir, expression, &mut |child| pending.push(child));
    }
    declared.retain(|slot| !assigned.contains(slot));
    declared
}

/// The initializer behind each fixed temporary — the value the call lowering spilled into the
/// slot. Only slots [`fixed_temporaries`] already proved the body never reassigns are keyed, so
/// the initializer is what every read of the slot observes, wherever the read stands.
fn fixed_temporary_inits(
    ir: &IrFile,
    roots: &[ExprId],
    fixed: &crate::fx_hash::HashSet<u32>,
) -> crate::fx_hash::HashMap<u32, ExprId> {
    let mut inits = crate::fx_hash::HashMap::default();
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Variable {
            index,
            init: Some(init),
            named: false,
            ..
        } = ir.expr(expression)
        {
            if fixed.contains(index) {
                inits.insert(*index, *init);
            }
        }
        for_each_frame_child(ir, expression, &mut |child| pending.push(child));
    }
    inits
}

/// The first value slot nothing in the body uses, where a loop step's temporaries start.
fn first_free_slot(ir: &IrFile, roots: &[ExprId], frame: &Frame) -> u32 {
    let mut free = frame.floor;
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let used = match ir.expr(expression) {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => Some(*index),
            IrExpr::Checked(checked_op)
                if let crate::ir::IrCheckedOperation::RangeLoop { variable, .. } =
                    &**checked_op =>
            {
                Some(*variable)
            }
            IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var).max(),
            _ => None,
        };
        if let Some(used) = used {
            free = free.max(used + 1);
        }
        for_each_frame_child(ir, expression, &mut |child| pending.push(child));
    }
    free
}

/// Whether one or several root-to-node paths reach each node in the body.
///
/// Every structural edge is counted, including edges the rewrite itself will not follow — a `try`'s
/// contents and a lambda's inline body. Sharing propagates through descendants: if two paths reach a
/// block, they also reach the return stored inside that block even though the return has only one
/// direct parent node. Counts are capped at two because the rewrite needs only a uniqueness proof.
fn root_path_counts(ir: &IrFile, roots: &[ExprId]) -> crate::fx_hash::HashMap<ExprId, u8> {
    let mut paths: crate::fx_hash::HashMap<ExprId, u8> = crate::fx_hash::HashMap::default();
    // A root is owned by the body itself, which is an edge like any other.
    for &root in roots {
        let count = paths.entry(root).or_default();
        *count = count.saturating_add(1).min(2);
    }
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut expanded = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        // A node declares its children once however many edges reach it; expanding it twice would
        // count the same edges again rather than find new ones.
        if !expanded.insert(expression) {
            continue;
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            let count = paths.entry(child).or_default();
            *count = count.saturating_add(1).min(2);
            pending.push(child);
        }
    }

    // Direct incoming-edge counts do not expose sharing THROUGH an ancestor: expanding a shared
    // block once records one edge to its child even though two root paths observe that child. Mark
    // every descendant of a multiply reached node multiply reached as well.
    let mut pending = paths
        .iter()
        .filter_map(|(&expression, &count)| (count > 1).then_some(expression))
        .collect::<Vec<_>>();
    let mut propagated = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !propagated.insert(expression) {
            continue;
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            paths.insert(child, 2);
            pending.push(child);
        }
    }
    paths
}

/// Whether a `return` leaves THIS function.
///
/// The checked depth is the authority, not the shape the `return` sits in: zero is a return of this
/// callable and a deeper one targets a frame outside it. A `return` lowering generated itself
/// carries no depth and is this function's by construction.
fn returns_from_here(ir: &IrFile, expression: ExprId) -> bool {
    ir.checked_return_depths
        .get(&expression)
        .is_none_or(|&depth| depth == 0)
}

/// The frame a `tailrec` loop steps: which function a self-call must name, and which value slots
/// hold the parameters it reassigns.
///
/// The slots are not always `0..count`. A body's dispatch receiver sits at `capture_count` with the
/// parameters after it, so a MEMBER's first parameter is one above its `this` — reassigning from
/// zero there would write the receiver and shift every argument by one.
pub(super) struct Frame {
    function: FunId,
    /// The function whose parameter list this frame's slots index: [`Self::function`], or the
    /// inline lambda's method, whose captures lead its list.
    numbering: FunId,
    /// The value slot holding each argument position of a self-call: the capture prefix, then the
    /// logical parameters. In an inline lambda's frame a position is `None` where the lambda does
    /// not capture the slot of the enclosing frame that holds it.
    slots: Vec<Option<u32>>,
    count: usize,
    /// Whether the function is a member, whose frame is its instance too. `false` for a static
    /// function, including an extension: an extension receiver is an ordinary parameter, which is
    /// what lets a self-call re-bind it by stepping.
    member: bool,
    /// Slots that hold this frame's own instance — the dispatch receiver (or, in an inline lambda's
    /// frame, the capture of it) plus the generated temporaries a call spills it into. Empty for a
    /// static frame. Filled by [`Frame::survey`].
    this_slots: crate::fx_hash::HashSet<u32>,
    /// The first value slot above the frame's parameters: an inline lambda's captures and
    /// parameters, or the function's own parameters.
    floor: u32,
    /// Whether this is the frame of an inline lambda's body, whose slots number from the lambda's
    /// own frame. A step there writes the captures of the function's parameters, which the
    /// inliner binds to the caller's own slots, and evaluates no default: a default reads the
    /// function's frame.
    inline_lambda: bool,
    /// Arguments a self-call passes AHEAD of the logical parameters: a LOCAL function's captures,
    /// which lifting added to its declaration and therefore to every call to it. The loop must
    /// leave those slots exactly as they are — a self-call re-reads the frame's own capture
    /// parameters, so the values are already what the next turn needs — and reassign only what
    /// follows. 0 for any frame that has no such prefix.
    capture_prefix: usize,
    /// The next value slot a loop step may take for a temporary. Set by [`finish_tailrec_body`]
    /// above every slot the body already uses.
    next_slot: std::cell::Cell<u32>,
    /// See [`fixed_temporaries`]. Filled by [`finish_tailrec_body`].
    fixed_temporaries: crate::fx_hash::HashSet<u32>,
    /// The initializer behind each slot of [`Self::fixed_temporaries`] — the value the call
    /// lowering spilled there. A self-call's operands are READS of these slots, so only through
    /// this map can the sweep see what a step would actually re-evaluate. Filled by
    /// [`finish_tailrec_body`].
    spill_inits: crate::fx_hash::HashMap<u32, ExprId>,
}

impl Frame {
    /// The frame a body reported, for the function it lowers and its parameter count.
    ///
    /// The slots come from [`super::BodySlots`] and are NOT recomputed here. Deriving them a second
    /// time is what this constructor exists to prevent: a static frame is not always slot zero and a
    /// member's parameters are not always `this + 1` — captures, a local class's constructor
    /// captures and its context values all take slots first, and only the lowering knows how many.
    pub(super) fn of_body(function: FunId, slots: super::BodySlots, count: usize) -> Self {
        let count_slots =
            u32::try_from(count).expect("tailrec parameter count exceeds packed value ids");
        Self {
            function,
            numbering: function,
            slots: (slots.first_parameter..slots.first_parameter + count_slots)
                .map(Some)
                .collect(),
            count,
            member: slots.dispatch_receiver.is_some(),
            this_slots: slots.dispatch_receiver.into_iter().collect(),
            floor: slots.first_parameter + count_slots,
            inline_lambda: false,
            capture_prefix: 0,
            next_slot: std::cell::Cell::new(0),
            fixed_temporaries: crate::fx_hash::HashSet::default(),
            spill_inits: crate::fx_hash::HashMap::default(),
        }
    }

    /// The frame of a LOCAL function, whose parameter list — and every call to it — leads with the
    /// values lifting captured. `count` is the LOGICAL parameters that follow: the context
    /// parameters, an extension receiver where there is one, then the declared value parameters.
    pub(super) fn of_local_body(function: FunId, slots: super::BodySlots, count: usize) -> Self {
        let mut frame = Self::of_body(function, slots, count);
        // The captures take the slots below the first parameter, each at its own position.
        frame
            .slots
            .splice(0..0, (0..slots.first_parameter).map(Some));
        frame.capture_prefix = slots.first_parameter as usize;
        frame
    }

    /// The frame of the body of an inline lambda written in `outer`, which captures `captures`
    /// and is implemented by `impl_fn`. The body numbers its slots from the lambda's frame,
    /// captures first: a slot of `outer` is reached through the capture that reads it.
    fn of_inline_body(
        ir: &IrFile,
        outer: &Frame,
        captures: &[ExprId],
        impl_fn: FunId,
        body: ExprId,
    ) -> Self {
        let captured = |outer_slot: u32| {
            captures
                .iter()
                .position(|&capture| {
                    matches!(ir.expr(capture), IrExpr::GetValue(slot) if *slot == outer_slot)
                })
                .and_then(|capture| u32::try_from(capture).ok())
        };
        let mut frame = Self {
            function: outer.function,
            numbering: impl_fn,
            slots: outer
                .slots
                .iter()
                .map(|slot| slot.and_then(captured))
                .collect(),
            count: outer.count,
            member: outer.member,
            this_slots: outer
                .this_slots
                .iter()
                .filter_map(|&slot| captured(slot))
                .collect(),
            floor: u32::try_from(ir.functions[impl_fn as usize].params.len())
                .expect("lambda parameter count exceeds packed value ids"),
            inline_lambda: true,
            capture_prefix: outer.capture_prefix,
            next_slot: std::cell::Cell::new(0),
            fixed_temporaries: crate::fx_hash::HashSet::default(),
            spill_inits: crate::fx_hash::HashMap::default(),
        };
        frame.survey(ir, &[body]);
        frame
    }

    /// Record what the body `roots` holds in this frame: the slots aliasing its instance, where a
    /// step's temporaries start, and the temporaries the body never reassigns.
    fn survey(&mut self, ir: &IrFile, roots: &[ExprId]) {
        collect_this_slots(ir, roots, self);
        self.next_slot.set(first_free_slot(ir, roots, self));
        self.fixed_temporaries = fixed_temporaries(ir, roots);
        self.spill_inits = fixed_temporary_inits(ir, roots, &self.fixed_temporaries);
    }

    /// The value slot of the parameter at `position` in a self-call's argument list, which counts
    /// the capture prefix. Only a frame that [`Self::holds_every_position`] steps a call.
    fn parameter_slot(&self, position: usize) -> u32 {
        self.slots[position].expect("a stepped frame holds every parameter")
    }

    fn is_parameter_slot(&self, slot: u32) -> bool {
        self.slots[self.capture_prefix..].contains(&Some(slot))
    }

    /// Whether every argument position has a slot in this frame, which a step writes or re-reads.
    fn holds_every_position(&self) -> bool {
        self.slots.iter().all(Option::is_some)
    }

    /// The parameters `outer` holds that this inline lambda's frame does not capture.
    fn uncaptured_parameters(&self, outer: &Frame) -> Vec<FrameParameter> {
        self.slots
            .iter()
            .zip(&outer.slots)
            .enumerate()
            .filter_map(|(position, (own, outer_slot))| {
                let slot = (*outer_slot).filter(|_| own.is_none())?;
                // A function's frame lists its parameters in argument order; an inline lambda's
                // lists each capture at its own slot.
                let index = if outer.inline_lambda {
                    slot as usize
                } else {
                    position
                };
                Some(FrameParameter {
                    slot,
                    numbering: outer.numbering,
                    index,
                })
            })
            .collect()
    }

    fn temporary(&self) -> u32 {
        let slot = self.next_slot.get();
        self.next_slot.set(slot + 1);
        slot
    }
}

/// Whether `call` is this function calling itself — the only shape the loop can step. Every
/// parameter the call leaves out must have a default the step can evaluate in its place (see
/// [`fillable_defaults`]).
///
/// For a MEMBER the frame is the instance too: `count(n - 1)` on `this` is the same frame and steps,
/// while `Other().count(n - 1)` is a different one and has to stay a call. A member of an `object`
/// or a companion has only one instance, so `O.rec(n - 1)` is the same frame whatever its receiver
/// expression says (kotlinc's `hasSameDispatchReceiver` for a singleton).
fn is_self_call(ir: &IrFile, call: ExprId, frame: &Frame) -> bool {
    match ir.expr(call) {
        IrExpr::Call {
            callee: call_callee,
            dispatch_receiver: None,
            args,
        } if let Callee::LocalWithDefaults {
            function: target,
            defaults,
        }
        | Callee::ClassStaticWithDefaults {
            function: target,
            defaults,
            ..
        } = &**call_callee =>
        {
            !frame.member
                && *target == frame.function
                && args.len() + defaults.len() == frame.capture_prefix + frame.count
                && passes_its_own_captures(ir, args, frame)
                && fillable_defaults(
                    ir,
                    frame,
                    defaults.iter().map(|&position| position as usize),
                )
        }
        // A local function declared inside a class member is lifted onto that class as a private
        // STATIC, so its self-call is a `ClassStatic` rather than a `Local`. It is the same
        // declaration and the same frame — the callee identity says so, not the owner's spelling —
        // and recognizing only `Local` left that shape recursing until `StackOverflowError`.
        IrExpr::Call {
            callee: call_callee,
            dispatch_receiver: None,
            args,
        } if let Callee::Local(target)
        | Callee::ClassStatic {
            function: target, ..
        } = &**call_callee =>
        {
            !frame.member
                && *target == frame.function
                && args.len() == frame.capture_prefix + frame.count
                && passes_its_own_captures(ir, args, frame)
        }
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } => {
            if !frame.member {
                return false;
            }
            let Some(owner) = ir.classes.get(*class as usize) else {
                return false;
            };
            let same_instance = match ir.expr(*receiver) {
                IrExpr::GetValue(slot) => owner.is_singleton() || frame.this_slots.contains(slot),
                IrExpr::StaticInstance { .. } | IrExpr::SingletonValue { .. } => {
                    owner.is_singleton()
                }
                _ => false,
            };
            owner.methods.get(*index as usize) == Some(&frame.function)
                && same_instance
                && args.len() == frame.capture_prefix + frame.count
                && fillable_defaults(
                    ir,
                    frame,
                    args.iter()
                        .enumerate()
                        .filter(|(_, argument)| argument.is_none())
                        .map(|(position, _)| position),
                )
        }
        _ => false,
    }
}

/// Whether the evaluation a loop step would REPEAT on every turn can suspend: the self-call's
/// receiver and supplied arguments, plus each omitted parameter's default the step evaluates in
/// its place.
///
/// A stepped call disappears, so its operand evaluation moves bodily into the loop turn: the
/// spilled temporaries it reads stay as statements of the turn, and the step's own parameter
/// stores follow them. A suspension in that region is one the JVM coroutine state machine would
/// have to split a state around in the middle of the turn's argument reassignment, a shape it
/// does not model — the resume lands past the reassignment the turn exists to perform. kotlinc
/// draws the same line from its own side: it loops a `tailrec suspend` self-call whose operands
/// are pure, and re-invokes through the continuation when a suspension sits among them
/// (`EscKt$escape$1.invokeSuspend` ends in a real `invokestatic EscKt.escape(…); areturn`).
///
/// Declining costs the loop, never the answer: the call stays the call the program already had.
/// A suspension OUTSIDE this region — a statement of its own earlier in the body — is not the
/// step's business; the state machine handles those inside the loop.
fn stepped_evaluation_suspends(ir: &IrFile, call: ExprId, frame: &Frame) -> bool {
    let args = self_call_arguments(ir, call);
    let defaults = ir.param_defaults(frame.function);
    args.iter().enumerate().any(|(position, argument)| {
        let operand = argument
            .or_else(|| defaults.and_then(|defaults| defaults.get(position).copied().flatten()));
        operand.is_some_and(|operand| evaluation_suspends(ir, operand, &frame.spill_inits))
    }) || match ir.expr(call) {
        IrExpr::Call {
            dispatch_receiver, ..
        } => dispatch_receiver
            .is_some_and(|receiver| evaluation_suspends(ir, receiver, &frame.spill_inits)),
        IrExpr::MethodCall { receiver, .. } => {
            evaluation_suspends(ir, *receiver, &frame.spill_inits)
        }
        _ => false,
    }
}

/// Whether evaluating `root` can suspend THIS function: a call to a suspend callable (same-file
/// through [`IrFile::suspend_funs`], cross-unit through the per-expression
/// [`IrFile::suspend_calls`]), or an intrinsic suspension point. These are the facts common
/// lowering records at each call site as it lowers the body, so they are complete by the time the
/// sweep runs and impose no declaration-order requirement.
///
/// A read of a spilled operand stands for the initializer the step re-evaluates, so the scan
/// follows it through [`Frame::spill_inits`] — the same spill discipline [`collect_this_slots`]
/// and [`is_self_call`] already rely on. A lambda's body is its own frame and cannot suspend the
/// caller mid-step; only its captures, which this function evaluates, are descended into.
fn evaluation_suspends(
    ir: &IrFile,
    root: ExprId,
    spill_inits: &crate::fx_hash::HashMap<u32, ExprId>,
) -> bool {
    let mut pending = vec![root];
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if ir.suspend_calls.contains_key(&expression)
            || ir.intrinsic_suspension_points.contains_key(&expression)
        {
            return true;
        }
        match ir.expr(expression) {
            IrExpr::Call { callee, .. } => {
                if callee
                    .source_function()
                    .is_some_and(|function| ir.suspend_funs.contains(&function))
                {
                    return true;
                }
            }
            IrExpr::MethodCall { class, index, .. } => {
                if ir
                    .classes
                    .get(*class as usize)
                    .and_then(|class| class.methods.get(*index as usize))
                    .is_some_and(|function| ir.suspend_funs.contains(function))
                {
                    return true;
                }
            }
            IrExpr::GetValue(slot) => {
                if let Some(&init) = spill_inits.get(slot) {
                    pending.push(init);
                }
                continue;
            }
            _ => {}
        }
        for_each_frame_child(ir, expression, &mut |child| pending.push(child));
    }
    false
}

/// Whether each omitted parameter at `positions` has a default the loop step can evaluate itself.
///
/// The step evaluates a default where the call would have, reading the NEW values of the
/// parameters before it (kotlinc's `TailrecLowering.genTailCall`). A default that reads its own or a
/// later parameter would read kotlinc's null-initialized placeholder, a shape the step does not
/// build, so such a call stays a call.
fn fillable_defaults(
    ir: &IrFile,
    frame: &Frame,
    mut positions: impl Iterator<Item = usize>,
) -> bool {
    if frame.inline_lambda {
        return positions.next().is_none();
    }
    let defaults = ir.param_defaults(frame.function);
    positions.into_iter().all(|position| {
        let Some(default) = defaults.and_then(|defaults| defaults.get(position).copied().flatten())
        else {
            return false;
        };
        let own = frame.parameter_slot(position);
        !reads_value(ir, default, &|slot| {
            frame.is_parameter_slot(slot) && slot >= own
        })
    })
}

/// The children of `expression` that read this frame's slots. A lambda's inline body numbers its
/// slots from the lambda's own frame, so only its captures belong to this one.
fn for_each_frame_child(ir: &IrFile, expression: ExprId, f: &mut impl FnMut(ExprId)) {
    match ir.expr(expression) {
        IrExpr::Lambda { captures, .. } => captures.iter().copied().for_each(f),
        _ => crate::ir::for_each_child(&ir.exprs, expression, f),
    }
}

fn reads_value(ir: &IrFile, root: ExprId, matches: &dyn Fn(u32) -> bool) -> bool {
    let mut pending = vec![root];
    let mut seen = crate::fx_hash::HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if matches!(ir.expr(expression), IrExpr::GetValue(slot) if matches(*slot)) {
            return true;
        }
        for_each_frame_child(ir, expression, &mut |child| pending.push(child));
    }
    false
}

/// Whether a self-call's capture prefix re-reads this frame's OWN capture parameters.
///
/// It always does when the call is what it looks like: inside the lifted function the captured
/// values ARE its leading parameters, and the call site reloads them. Checking it is what makes
/// dropping those arguments sound — the loop step does not reassign a capture slot, so an argument
/// that was anything but a re-read of the slot it fills would be lost.
fn passes_its_own_captures(ir: &IrFile, args: &[ExprId], frame: &Frame) -> bool {
    args.iter().take(frame.capture_prefix).enumerate().all(|(position, argument)| {
        matches!(ir.expr(*argument), IrExpr::GetValue(source) if frame.slots[position] == Some(*source))
    })
}

/// The arguments a self-call passes, in parameter order, with `None` for each one it leaves to
/// its default. `call` must satisfy [`is_self_call`].
fn self_call_arguments(ir: &IrFile, call: ExprId) -> Vec<Option<ExprId>> {
    match ir.expr(call) {
        IrExpr::Call {
            callee: call_callee,
            args,
            ..
        } if let Callee::LocalWithDefaults { defaults, .. }
        | Callee::ClassStaticWithDefaults { defaults, .. } = &**call_callee =>
        {
            let mut supplied = args.iter().copied();
            (0..args.len() + defaults.len())
                .map(|position| {
                    if defaults.contains(&(position as u32)) {
                        None
                    } else {
                        supplied.next()
                    }
                })
                .collect()
        }
        IrExpr::Call { args, .. } => args.iter().copied().map(Some).collect(),
        IrExpr::MethodCall { args, .. } => args.clone(),
        _ => unreachable!("a self call is a call"),
    }
}

/// The loop step a self-call becomes: every parameter reassigned, then `continue` to the synthetic
/// loop. `call` must satisfy [`is_self_call`].
///
/// The receiver is deliberately NOT reassigned. A static frame has none, and a member self-call is
/// the same frame only when it already dispatches on `this` — so the slot holding it is already
/// correct, and writing it would be a store with nothing to store.
fn loop_step(
    ir: &mut IrFile,
    call: ExprId,
    line: Option<u32>,
    frame: &Frame,
    origin: OriginId,
) -> IrExpr {
    let args = self_call_arguments(ir, call);
    let mut updates = Vec::with_capacity(frame.count + 1);
    // The capture prefix is skipped, not written: those slots already hold what the next turn
    // reads, and `is_self_call` established that the dropped arguments are re-reads of them.
    // kotlinc builds the step's temporaries at the call, so each store is on the call's line.
    let held = Held {
        frame,
        line,
        origin,
    };
    if let IrExpr::MethodCall {
        receiver, class, ..
    } = *ir.expr(call)
    {
        let ty = Ty::obj_name(ir.classes[class as usize].fq_name);
        hold_receiver(ir, receiver, ty, &held, &mut updates);
    }
    let assignments = step_assignments(ir, &args, &held, &mut updates);
    for (position, value) in assignments {
        updates.push(generated(
            ir,
            IrExpr::SetValue {
                var: frame.parameter_slot(position),
                value,
            },
            origin,
        ));
    }
    updates.push(generated(
        ir,
        IrExpr::Continue {
            label: Some(LOOP_LABEL.to_owned()),
        },
        origin,
    ));
    IrExpr::Block {
        stmts: updates,
        value: None,
    }
}

/// A member self-call's receiver is evaluated and held like any argument, and then never written
/// back: the frame's instance does not change. kotlinc holds a receiver other than `this` (an
/// `object`'s own instance read) in a temporary nothing reads.
fn hold_receiver(
    ir: &mut IrFile,
    receiver: ExprId,
    ty: Ty,
    held: &Held<'_>,
    statements: &mut Vec<ExprId>,
) {
    if matches!(ir.expr(receiver), IrExpr::GetValue(slot) if held.frame.this_slots.contains(slot)) {
        return;
    }
    held.declare(ir, ty, receiver, statements);
}

/// Where a loop step declares its temporaries.
struct Held<'a> {
    frame: &'a Frame,
    /// The self-call's source line.
    line: Option<u32>,
    origin: OriginId,
}

impl Held<'_> {
    /// A temporary holding `init`, declared onto `statements`; its slot.
    fn declare(&self, ir: &mut IrFile, ty: Ty, init: ExprId, statements: &mut Vec<ExprId>) -> u32 {
        let slot = self.frame.temporary();
        let declaration = generated(
            ir,
            IrExpr::Variable {
                index: slot,
                ty,
                init: Some(init),
                named: false,
            },
            self.origin,
        );
        if let Some(line) = self.line {
            ir.expr_lines.insert(declaration, line);
        }
        ir.call_operand_bindings.insert(declaration);
        statements.push(declaration);
        slot
    }
}

/// kotlinc's `genTailCall`: every supplied argument is held in a temporary, each omitted
/// parameter's default is then evaluated in order over the NEW values of the parameters before it,
/// and only after that are the parameters written — supplied ones first, then defaulted ones, each
/// in parameter order. The temporaries declared along the way are pushed onto `statements`; the
/// assignments are returned.
///
/// A temporary kotlinc's `JvmOptimizationLowering` would remove is not made: a constant is used
/// where it is read, and so is a read of a binding that never changes. A parameter changes — the
/// loop writes it — so a read of one is held like any other value.
fn step_assignments(
    ir: &mut IrFile,
    args: &[Option<ExprId>],
    held: &Held<'_>,
    statements: &mut Vec<ExprId>,
) -> Vec<(usize, ExprId)> {
    let (frame, origin) = (held.frame, held.origin);
    /// Where the next turn's value of a parameter is: a temporary's slot, or a constant
    /// expression that is repeated wherever it is read.
    #[derive(Clone, Copy)]
    enum NewValue {
        Slot(u32),
        Constant(ExprId),
    }
    let parameter_types = ir.functions[frame.function as usize].params.clone();
    let mut declare = |ir: &mut IrFile, position: usize, init: ExprId| {
        NewValue::Slot(held.declare(ir, parameter_types[position], init, statements))
    };
    let mut new_values: Vec<(usize, NewValue)> = Vec::new();
    for (position, argument) in args.iter().enumerate().skip(frame.capture_prefix) {
        let Some(argument) = *argument else {
            continue;
        };
        let value = match ir.expr(argument) {
            _ if is_constant(ir, argument) => NewValue::Constant(argument),
            IrExpr::GetValue(slot)
                if !frame.is_parameter_slot(*slot)
                    && (frame.fixed_temporaries.contains(slot)
                        || ir.binding_read_stability.get(&argument)
                            == Some(&crate::ir::IrBindingStability::Stable)) =>
            {
                NewValue::Slot(*slot)
            }
            _ => declare(ir, position, argument),
        };
        new_values.push((position, value));
    }
    let defaults = ir
        .param_defaults(frame.function)
        .cloned()
        .unwrap_or_default();
    let parameters_end = frame.floor;
    for (position, _) in args
        .iter()
        .enumerate()
        .skip(frame.capture_prefix)
        .filter(|(_, argument)| argument.is_none())
    {
        let default = defaults[position].expect("checked: the omitted parameter has a default");
        let (copy, _) = crate::ir::clone_expression_dag(ir, default);
        // The default's own locals move above everything the body uses; its parameter reads stay
        // put for the substitution below.
        let identity = (0..parameters_end).collect::<Vec<_>>();
        let locals = super::source_calls::rehome_inline_body_values(
            ir,
            copy,
            &identity,
            frame.next_slot.get(),
        )
        .expect("value slots stay within packed ids");
        frame.next_slot.set(frame.next_slot.get() + locals);
        substitute_parameter_reads(ir, copy, frame, &new_values);
        let value = if is_constant(ir, copy) {
            // kotlinc reads the constant where its temporary was read, at the call's position.
            forget_lines(ir, copy);
            NewValue::Constant(copy)
        } else {
            declare(ir, position, copy)
        };
        new_values.push((position, value));
    }

    fn substitute_parameter_reads(
        ir: &mut IrFile,
        root: ExprId,
        frame: &Frame,
        new_values: &[(usize, NewValue)],
    ) {
        let mut pending = vec![root];
        let mut seen = crate::fx_hash::HashSet::default();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if let IrExpr::GetValue(slot) = *ir.expr(expression) {
                let replacement = new_values
                    .iter()
                    .find(|(position, _)| frame.parameter_slot(*position) == slot);
                match replacement {
                    Some((_, NewValue::Slot(new))) => {
                        ir.exprs[expression as usize] = IrExpr::GetValue(*new);
                    }
                    Some((_, NewValue::Constant(constant))) => {
                        let (copy, _) = crate::ir::clone_expression_dag(ir, *constant);
                        ir.exprs[expression as usize] = ir.expr(copy).clone();
                    }
                    None => {}
                }
                continue;
            }
            for_each_frame_child(ir, expression, &mut |child| pending.push(child));
        }
    }

    new_values
        .into_iter()
        .map(|(position, value)| {
            let read = match value {
                NewValue::Slot(slot) => generated(ir, IrExpr::GetValue(slot), origin),
                NewValue::Constant(constant) => constant,
            };
            (position, read)
        })
        .collect()
}

fn forget_lines(ir: &mut IrFile, root: ExprId) {
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        ir.expr_lines.remove(&expression);
        ir.expr_source_lines.remove(&expression);
        ir.expr_end_lines.remove(&expression);
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

/// A constant, possibly widened to a reference type without changing its representation: the
/// initializer kotlinc's `JvmOptimizationLowering` inlines into a temporary's reads.
fn is_constant(ir: &IrFile, expression: ExprId) -> bool {
    match ir.expr(expression) {
        IrExpr::Const(_) => true,
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } => {
            type_operand.is_reference()
                && matches!(
                    ir.expr(*arg),
                    IrExpr::Const(IrConst::String(_) | IrConst::Null)
                )
        }
        _ => false,
    }
}

fn generated(ir: &mut IrFile, expression: IrExpr, cause: OriginId) -> ExprId {
    let id = ir.add_expr(expression);
    ir.fir_origins.insert(
        id,
        IrNodeOrigin::Synthetic {
            cause,
            kind: SyntheticOriginKind::GeneratedControlFlow,
        },
    );
    id
}

#[cfg(test)]
mod tests;
