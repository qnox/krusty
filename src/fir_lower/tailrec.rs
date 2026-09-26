//! Tail-position rewriting for checked self calls.
//!
//! Calls reach this module only after FIR has selected a stable callable identity and mapped every
//! argument to its declaration parameter. The transform therefore contains no name lookup or
//! overload logic.

use crate::fir::{OriginId, SyntheticOriginKind};
use crate::ir::{Callee, ExprId, FunId, IrConst, IrExpr, IrFile, IrNodeOrigin};
use crate::types::Ty;

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
    collect_this_slots(ir, &roots, &mut frame);
    frame.next_slot.set(first_free_slot(ir, &roots, &frame));
    frame.fixed_temporaries = fixed_temporaries(ir, &roots);
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
                ir.implicit_return_end_lines.insert(returned, end);
            }
            roots.push(returned);
        }
    }
    let tail_calls = tail_calls(ir, &roots, frame, unit);
    if tail_calls.is_empty() {
        return super::finish_callable_body(
            ir,
            roots,
            result,
            unit && implicit_return,
            false,
            origin,
        );
    }
    for TailCall { edge, call, line } in tail_calls {
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
/// entered — its `finally` runs after the value — and neither is a lambda's body, whose returns are
/// the lambda's. A `return` or coercion that holds the call directly is replaced along with it: the
/// step leaves the loop turn and produces no value for either.
///
/// Only a node reached by exactly ONE path is rewritten. A call below a shared ancestor is seen by
/// another path too, which may cross a boundary this walk does not enter, so it stays a call.
fn tail_calls(ir: &IrFile, roots: &[ExprId], frame: &Frame, unit: bool) -> Vec<TailCall> {
    struct Walk<'a> {
        ir: &'a IrFile,
        frame: &'a Frame,
        unit: bool,
        paths: std::collections::HashMap<ExprId, u8>,
        found: Vec<TailCall>,
        seen: std::collections::HashSet<ExprId>,
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
                IrExpr::Lambda { captures, .. } => {
                    for capture in captures.clone() {
                        self.visit(capture, false, None);
                    }
                }
                IrExpr::Call { .. } | IrExpr::MethodCall { .. } => {
                    self.children(expression);
                    if let Some(edge) = edge.filter(|_| tail && self.is_self_call(expression)) {
                        self.found.push(TailCall {
                            edge,
                            call: expression,
                            line: self.line,
                        });
                    }
                }
                IrExpr::Return(Some(value)) => {
                    let value = *value;
                    let holder = edge.or(Some(Edge::Return(expression)));
                    self.visit_held(value, returns_from_here(self.ir, expression), holder);
                }
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => {
                    let arg = *arg;
                    self.visit_held(arg, tail, edge);
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

        fn is_self_call(&self, call: ExprId) -> bool {
            self.paths.get(&call) == Some(&1) && is_self_call(self.ir, call, self.frame)
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
            returns_from_here(self.ir, statement)
                && match self.ir.expr(statement) {
                    IrExpr::Return(None) => true,
                    IrExpr::Return(Some(value)) => {
                        matches!(self.ir.expr(*value), IrExpr::UnitInstance)
                    }
                    _ => false,
                }
        }
    }

    let mut walk = Walk {
        ir,
        frame,
        unit,
        paths: root_path_counts(ir, roots),
        found: Vec::new(),
        seen: std::collections::HashSet::new(),
        line: None,
    };
    for (index, &root) in roots.iter().enumerate() {
        let tail = match roots.get(index + 1) {
            Some(&next) => unit && walk.returns_unit(next),
            None => true,
        };
        walk.visit(root, tail, Some(Edge::Root(index)));
    }
    walk.found
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
    if frame.receiver.is_none() {
        return;
    }
    let mut bindings: Vec<(u32, ExprId)> = Vec::new();
    let mut assigned: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = std::collections::HashSet::new();
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
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
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
fn fixed_temporaries(ir: &IrFile, roots: &[ExprId]) -> std::collections::HashSet<u32> {
    let mut declared = std::collections::HashSet::new();
    let mut assigned = std::collections::HashSet::new();
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = std::collections::HashSet::new();
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
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    declared.retain(|slot| !assigned.contains(slot));
    declared
}

/// The first value slot nothing in the body uses, where a loop step's temporaries start.
fn first_free_slot(ir: &IrFile, roots: &[ExprId], frame: &Frame) -> u32 {
    let parameters_end = frame.parameter_slot(frame.capture_prefix + frame.count);
    let mut free = parameters_end;
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let used = match ir.expr(expression) {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => Some(*index),
            IrExpr::Checked(crate::ir::IrCheckedOperation::RangeLoop { variable, .. }) => {
                Some(*variable)
            }
            IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var).max(),
            _ => None,
        };
        if let Some(used) = used {
            free = free.max(used + 1);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    free
}

/// Whether one or several root-to-node paths reach each node in the body.
///
/// Every structural edge is counted, including edges the rewrite itself will not follow — a `try`'s
/// contents and a lambda's inline body. Sharing propagates through descendants: if two paths reach a
/// block, they also reach the return stored inside that block even though the return has only one
/// direct parent node. Counts are capped at two because the rewrite needs only a uniqueness proof.
fn root_path_counts(ir: &IrFile, roots: &[ExprId]) -> std::collections::HashMap<ExprId, u8> {
    let mut paths: std::collections::HashMap<ExprId, u8> = std::collections::HashMap::new();
    // A root is owned by the body itself, which is an edge like any other.
    for &root in roots {
        let count = paths.entry(root).or_default();
        *count = count.saturating_add(1).min(2);
    }
    let mut pending: Vec<ExprId> = roots.to_vec();
    let mut expanded = std::collections::HashSet::new();
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
    let mut propagated = std::collections::HashSet::new();
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
    /// Value slot of the first parameter.
    first_parameter: u32,
    count: usize,
    /// A member's `this` slot. `None` for a static function, including an extension: an extension
    /// receiver is an ordinary parameter inside `first_parameter..count`, which is what lets a
    /// self-call re-bind it by stepping.
    receiver: Option<u32>,
    /// Slots that hold this frame's own instance — `receiver` plus the generated temporaries a call
    /// spills it into. Empty for a static frame. Filled by [`finish_tailrec_body`].
    this_slots: std::collections::HashSet<u32>,
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
    fixed_temporaries: std::collections::HashSet<u32>,
}

impl Frame {
    /// The frame a body reported, for the function it lowers and its parameter count.
    ///
    /// The slots come from [`super::BodySlots`] and are NOT recomputed here. Deriving them a second
    /// time is what this constructor exists to prevent: a static frame is not always slot zero and a
    /// member's parameters are not always `this + 1` — captures, a local class's constructor
    /// captures and its context values all take slots first, and only the lowering knows how many.
    pub(super) fn of_body(function: FunId, slots: super::BodySlots, count: usize) -> Self {
        Self {
            function,
            first_parameter: slots.first_parameter,
            count,
            receiver: slots.dispatch_receiver,
            this_slots: slots.dispatch_receiver.into_iter().collect(),
            capture_prefix: 0,
            next_slot: std::cell::Cell::new(0),
            fixed_temporaries: std::collections::HashSet::new(),
        }
    }

    /// The frame of a LOCAL function, whose parameter list — and every call to it — leads with the
    /// values lifting captured. `count` is the LOGICAL parameters that follow: the context
    /// parameters, an extension receiver where there is one, then the declared value parameters.
    pub(super) fn of_local_body(function: FunId, slots: super::BodySlots, count: usize) -> Self {
        Self {
            capture_prefix: slots.first_parameter as usize,
            ..Self::of_body(function, slots, count)
        }
    }

    /// The value slot of the parameter at `position` in a self-call's argument list, which counts
    /// the capture prefix: captures take the slots below `first_parameter`.
    fn parameter_slot(&self, position: usize) -> u32 {
        let logical = u32::try_from(position - self.capture_prefix)
            .expect("tailrec parameter count exceeds packed value ids");
        self.first_parameter + logical
    }

    fn is_parameter_slot(&self, slot: u32) -> bool {
        (self.first_parameter..self.parameter_slot(self.capture_prefix + self.count))
            .contains(&slot)
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
            callee:
                Callee::LocalWithDefaults {
                    function: target,
                    defaults,
                }
                | Callee::ClassStaticWithDefaults {
                    function: target,
                    defaults,
                    ..
                },
            dispatch_receiver: None,
            args,
        } => {
            frame.receiver.is_none()
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
            callee:
                Callee::Local(target)
                | Callee::ClassStatic {
                    function: target, ..
                },
            dispatch_receiver: None,
            args,
        } => {
            frame.receiver.is_none()
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
            if frame.receiver.is_none() {
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

/// Whether each omitted parameter at `positions` has a default the loop step can evaluate itself.
///
/// The step evaluates a default where the call would have, reading the NEW values of the
/// parameters before it (kotlinc's `TailrecLowering.genTailCall`). A default that reads its own or a
/// later parameter would read kotlinc's null-initialized placeholder, a shape the step does not
/// build, so such a call stays a call.
fn fillable_defaults(ir: &IrFile, frame: &Frame, positions: impl Iterator<Item = usize>) -> bool {
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
    let mut seen = std::collections::HashSet::new();
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
    args.iter().take(frame.capture_prefix).enumerate().all(
        |(slot, argument)| matches!(ir.expr(*argument), IrExpr::GetValue(source) if *source as usize == slot),
    )
}

/// The arguments a self-call passes, in parameter order, with `None` for each one it leaves to
/// its default. `call` must satisfy [`is_self_call`].
fn self_call_arguments(ir: &IrFile, call: ExprId) -> Vec<Option<ExprId>> {
    match ir.expr(call) {
        IrExpr::Call {
            callee:
                Callee::LocalWithDefaults { defaults, .. }
                | Callee::ClassStaticWithDefaults { defaults, .. },
            args,
            ..
        } => {
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
    let parameters_end = frame.parameter_slot(frame.capture_prefix + frame.count);
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
        let mut seen = std::collections::HashSet::new();
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
mod tests {
    use super::*;
    use crate::ir::{IrFunction, IrNodeOrigin};

    const FUNCTION: FunId = 0;

    /// A one-parameter `tailrec fun step(n: Int): Int` with nothing in it yet. The body is supplied
    /// per test, because what each test is about is the SHAPE the sweep is handed.
    fn file() -> IrFile {
        let mut ir = IrFile::default();
        ir.add_fun(IrFunction {
            name: "step".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Int,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir
    }

    /// `return step(n)` — the shape the rewrite turns into a loop step.
    fn returned_self_call(ir: &mut IrFile) -> ExprId {
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(FUNCTION),
            dispatch_receiver: None,
            args: vec![argument],
        });
        ir.add_expr(IrExpr::Return(Some(call)))
    }

    fn finish(ir: &mut IrFile, roots: Vec<ExprId>) {
        finish_tailrec_body(
            ir,
            roots,
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: None,
                    first_parameter: 0,
                },
                1,
            ),
            false,
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");
    }

    /// A one-parameter MEMBER `tailrec fun step(n: Int): Int` on a class that declares it, with
    /// `this` in slot 0 and the parameter in slot 1 — the layout a member body is lowered with.
    fn member_file() -> IrFile {
        let mut ir = file();
        ir.functions[FUNCTION as usize].is_static = false;
        ir.functions[FUNCTION as usize].dispatch_receiver = Some(crate::types::type_name("C"));
        let class = ir.add_class(crate::ir::IrClass::synthetic(crate::types::type_name("C")));
        ir.classes[class as usize].methods.push(FUNCTION);
        ir
    }

    /// `{ t = <receiver>; this.step(n) }` returned — the shape a member call is lowered into, with
    /// the receiver spilled into temporary slot `temp`.
    fn returned_member_call(ir: &mut IrFile, receiver: ExprId, temp: u32) -> ExprId {
        let binding = ir.add_expr(IrExpr::Variable {
            index: temp,
            ty: Ty::Obj(crate::types::type_name("C"), &[]),
            init: Some(receiver),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(temp));
        let argument = ir.add_expr(IrExpr::GetValue(1));
        let call = ir.add_expr(IrExpr::MethodCall {
            class: 0,
            index: 0,
            receiver: read,
            args: vec![Some(argument)],
        });
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![binding],
            value: Some(call),
        });
        ir.add_expr(IrExpr::Return(Some(block)))
    }

    fn finish_member(ir: &mut IrFile, roots: Vec<ExprId>) {
        finish_tailrec_body(
            ir,
            roots,
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: Some(0),
                    first_parameter: 1,
                },
                1,
            ),
            false,
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");
    }

    /// Whether the rewrite produced a LOOP STEP anywhere in the body.
    ///
    /// A `continue` carrying the synthetic loop's label is written by `loop_step` and by nothing
    /// else.
    fn steps(ir: &IrFile) -> bool {
        ir.exprs.iter().any(|expression| {
            matches!(expression, IrExpr::Continue { label: Some(label) } if label == LOOP_LABEL)
        })
    }

    /// `return step(n)` whose value reaches the return through a coercion (an erased result, an
    /// elvis operand widened to the declared type) is still a tail call. The step takes the
    /// RETURN's place: no coercion is left holding it, and nothing is returned.
    #[test]
    fn a_self_call_under_a_coercion_replaces_its_return() {
        let mut ir = file();
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(FUNCTION),
            dispatch_receiver: None,
            args: vec![argument],
        });
        let coerced = ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: Ty::nullable(Ty::Int),
        });
        let returned = ir.add_expr(IrExpr::Return(Some(coerced)));
        ir.checked_return_depths.insert(returned, 0);
        let tail = ir.add_expr(IrExpr::Return(None));
        let body = finish_tailrec_body(
            &mut ir,
            vec![returned, tail],
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: None,
                    first_parameter: 0,
                },
                1,
            ),
            false,
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");

        assert!(steps(&ir), "the coerced self call is the loop step");
        let mut reachable = vec![body];
        let mut seen = std::collections::HashSet::new();
        while let Some(expression) = reachable.pop() {
            if seen.insert(expression) {
                crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                    reachable.push(child)
                });
            }
        }
        assert!(
            !seen.contains(&returned) && !seen.contains(&coerced) && !seen.contains(&call),
            "the step took the place of the return, its coercion and the call"
        );
    }

    /// The control for the two member tests below: a receiver spilled from `this` is still `this`,
    /// so the call is the same frame and steps. No Kotlin source reaches the rewrite WITHOUT this
    /// spill, so a member rewrite that did not follow the alias would never fire at all.
    #[test]
    fn a_receiver_spilled_from_this_is_still_this() {
        let mut ir = member_file();
        let this = ir.add_expr(IrExpr::GetValue(0));
        let returned = returned_member_call(&mut ir, this, 2);
        ir.checked_return_depths.insert(returned, 0);
        // A trailing statement keeps `returned` off the body's last root, so what rewrites it is
        // the SWEEP, in place — the same path the static control above takes.
        let tail = ir.add_expr(IrExpr::Return(None));
        finish_member(&mut ir, vec![returned, tail]);

        assert!(
            steps(&ir),
            "a self call on a temporary holding `this` is the loop step"
        );
    }

    /// A temporary the body REASSIGNS is not a reliable alias: what it held where the alias was
    /// established is not what it holds at the call. The slot is dropped and the call keeps
    /// recursing, which is the answer the program had before.
    #[test]
    fn a_reassigned_temporary_is_not_an_alias_for_this() {
        let mut ir = member_file();
        let this = ir.add_expr(IrExpr::GetValue(0));
        let returned = returned_member_call(&mut ir, this, 2);
        ir.checked_return_depths.insert(returned, 0);
        let other = ir.add_expr(IrExpr::GetValue(1));
        let overwrite = ir.add_expr(IrExpr::SetValue {
            var: 2,
            value: other,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish_member(&mut ir, vec![overwrite, returned, tail]);

        assert!(
            !steps(&ir),
            "a reassigned slot is not followed, so the call stays a call"
        );
    }

    /// The loop writes the slots the BODY reported, not slots derived from the receiver.
    ///
    /// `first_parameter` is not always `0` and not always `this + 1`. Captures, a local class's
    /// constructor captures and its context values all take slots before the parameters, so a body
    /// can put `this` at 3 and its first parameter at 7. Re-deriving the layout here — the thing
    /// `BodySlots` exists to stop — would write slot 4 and shift every argument by three, which is
    /// a miscompile no source-level test in this file would show, because the shapes that produce a
    /// prefix are the ones that decline for other reasons today.
    ///
    /// So this asserts on the store the step emits: reported slot in, same slot out.
    #[test]
    fn the_loop_step_writes_the_reported_parameter_slots() {
        const RECEIVER: u32 = 3;
        const FIRST_PARAMETER: u32 = 7;

        let mut ir = member_file();
        let this = ir.add_expr(IrExpr::GetValue(RECEIVER));
        let binding = ir.add_expr(IrExpr::Variable {
            index: 11,
            ty: Ty::Obj(crate::types::type_name("C"), &[]),
            init: Some(this),
            named: false,
        });
        let read = ir.add_expr(IrExpr::GetValue(11));
        let argument = ir.add_expr(IrExpr::GetValue(FIRST_PARAMETER));
        let call = ir.add_expr(IrExpr::MethodCall {
            class: 0,
            index: 0,
            receiver: read,
            args: vec![Some(argument)],
        });
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![binding],
            value: Some(call),
        });
        let returned = ir.add_expr(IrExpr::Return(Some(block)));
        ir.checked_return_depths.insert(returned, 0);
        let tail = ir.add_expr(IrExpr::Return(None));

        finish_tailrec_body(
            &mut ir,
            vec![returned, tail],
            Frame::of_body(
                FUNCTION,
                crate::fir_lower::BodySlots {
                    dispatch_receiver: Some(RECEIVER),
                    first_parameter: FIRST_PARAMETER,
                },
                1,
            ),
            false,
            OriginId::from_raw(0),
        )
        .expect("the body has a tail");

        assert!(steps(&ir), "the spilled receiver is still this frame");
        let written: Vec<u32> = ir
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::SetValue { var, .. } => Some(*var),
                _ => None,
            })
            .collect();
        assert_eq!(
            written,
            vec![FIRST_PARAMETER],
            "the step must write the reported first-parameter slot, and must not write the receiver"
        );
    }

    /// A call on an instance the frame did not supply is a different frame however it was reached,
    /// so it stays a call — this is what stops `Other().step(n)` from being stepped as if it were
    /// `this`.
    #[test]
    fn a_receiver_that_is_not_this_is_a_different_frame() {
        let mut ir = member_file();
        let other = ir.add_expr(IrExpr::GetValue(1));
        let returned = returned_member_call(&mut ir, other, 2);
        ir.checked_return_depths.insert(returned, 0);
        // A trailing statement keeps `returned` off the body's last root, so what rewrites it is
        // the SWEEP, in place — the same path the static control above takes.
        let tail = ir.add_expr(IrExpr::Return(None));
        finish_member(&mut ir, vec![returned, tail]);

        assert!(
            !steps(&ir),
            "a self call on another instance is not this frame"
        );
    }

    /// Every node carrying a checked return depth is still a `Return`, which is the side table's
    /// documented contract.
    fn depths_describe_returns(ir: &IrFile) -> bool {
        ir.checked_return_depths
            .keys()
            .all(|&expression| matches!(ir.expr(expression), IrExpr::Return(_)))
    }

    #[test]
    fn a_bare_return_retargets_the_block_edge_without_retyping_the_selected_call() {
        let mut ir = file();
        ir.functions[FUNCTION as usize].ret = Ty::Unit;
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(FUNCTION),
            dispatch_receiver: None,
            args: vec![argument],
        });
        ir.logical_types.insert(call, Ty::Unit);
        let returned = ir.add_expr(IrExpr::Return(None));
        let nested = ir.add_expr(IrExpr::Block {
            stmts: vec![call, returned],
            value: None,
        });
        let tail = ir.add_expr(IrExpr::Return(None));

        finish(&mut ir, vec![nested, tail]);

        let IrExpr::Block { stmts, value: None } = ir.expr(nested) else {
            panic!("the nested statement block remains")
        };
        assert_eq!(stmts.len(), 2);
        let step = stmts[0];
        assert_ne!(step, call);
        assert!(matches!(ir.expr(call), IrExpr::Call { .. }));
        assert_eq!(ir.logical_types.get(&call), Some(&Ty::Unit));
        assert!(matches!(
            ir.fir_origins.get(&step),
            Some(IrNodeOrigin::Synthetic {
                kind: SyntheticOriginKind::GeneratedControlFlow,
                ..
            })
        ));
    }

    #[test]
    fn a_return_reached_by_one_path_becomes_a_loop_step() {
        // The control for the two tests below: with nothing else pointing at it, the `return` is
        // rewritten, so a later "left alone" is the guard working and not the rewrite being off.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 0);
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![returned, tail]);

        assert!(
            steps(&ir),
            "a uniquely owned `return` of a self call is the loop step"
        );
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_the_dag_shares_with_a_try_is_left_alone() {
        // Common IR is a DAG. This `return` is reachable both as an ordinary statement and from
        // inside a `try`, which the sweep deliberately does not enter — so rewriting it through
        // the edge the sweep DOES follow would change what the `try` holds, where a `finally` still
        // has to run. One edge is the whole licence to rewrite in place, and there are two here.
        let mut ir = file();
        let shared = returned_self_call(&mut ir);
        let guarded = ir.add_expr(IrExpr::Try {
            body: shared,
            catches: Vec::new(),
            finally: None,
            result: Ty::Int,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![shared, guarded, tail]);

        assert!(
            matches!(ir.expr(shared), IrExpr::Return(Some(_))),
            "a `return` the DAG shares is left as it was"
        );
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_below_a_shared_ancestor_is_left_alone() {
        // The return has one DIRECT parent (the block), but the block itself is reached both as an
        // ordinary root and through an opaque `try`. Path uniqueness must propagate through the
        // shared ancestor; direct incoming-edge counting alone incorrectly rewrites the return.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 0);
        let shared_block = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        let guarded = ir.add_expr(IrExpr::Try {
            body: shared_block,
            catches: Vec::new(),
            finally: None,
            result: Ty::Int,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![shared_block, guarded, tail]);

        assert!(matches!(ir.expr(returned), IrExpr::Return(Some(_))));
        assert_eq!(ir.checked_return_depths.get(&returned), Some(&0));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_under_a_privately_owned_ancestor_is_still_swept() {
        // The control for the test above, and the one a descendant-marking rule needs most: the
        // same nesting with nothing else pointing at the block. One path reaches the return, so it
        // becomes the loop step. Without this, marking EVERY descendant of every node shared would
        // pass the test above while quietly switching the sweep off for anything inside a block.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 0);
        let owned = ir.add_expr(IrExpr::Block {
            stmts: vec![returned],
            value: None,
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![owned, tail]);

        let IrExpr::Block { stmts, .. } = ir.expr(owned) else {
            panic!("the owning block remains")
        };
        assert_ne!(
            stmts[0], returned,
            "one path to the `return` is the licence to replace it where it stands"
        );
        assert!(steps(&ir));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_lambdas_capture_is_swept_and_its_inline_body_is_not() {
        // The two halves of one boundary. A lambda's captures are evaluated by THIS function, so a
        // `return` among them is this function's tail position; its inline body belongs to the
        // lambda, and a depth-ZERO return there is the lambda's own — the one shape the checked
        // depth cannot tell apart from this function's, which is why the boundary and not the depth
        // is what keeps the sweep out of an inline body.
        let mut ir = file();
        let capture = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(capture, 0);
        let inner = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(inner, 0);
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: FUNCTION,
            arity: 0,
            captures: vec![capture],
            sam: None,
            inline_body: Some(inner),
        });
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![lambda, tail]);

        assert!(
            matches!(ir.expr(capture), IrExpr::Block { .. }),
            "a capture expression is this function's and is swept"
        );
        assert!(
            matches!(ir.expr(inner), IrExpr::Return(Some(_))),
            "an inline body is the lambda's and is left whole"
        );
        assert_eq!(ir.checked_return_depths.get(&inner), Some(&0));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_return_that_targets_an_outer_frame_is_left_alone() {
        // The ownership test is the checked DEPTH, not the shape the `return` was found in: a
        // non-zero depth names a frame outside this one, and stepping this function's loop would
        // answer a question nobody asked.
        let mut ir = file();
        let returned = returned_self_call(&mut ir);
        ir.checked_return_depths.insert(returned, 1);
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![returned, tail]);

        assert!(matches!(ir.expr(returned), IrExpr::Return(Some(_))));
        assert_eq!(ir.checked_return_depths.get(&returned), Some(&1));
        assert!(depths_describe_returns(&ir));
    }

    #[test]
    fn a_generated_node_records_where_it_came_from() {
        let mut ir = file();
        let tail = ir.add_expr(IrExpr::Return(None));
        finish(&mut ir, vec![tail]);
        let generated = ir.exprs.len() as ExprId - 1;
        assert!(matches!(
            ir.fir_origins.get(&generated),
            Some(IrNodeOrigin::Synthetic { .. })
        ));
    }
}
