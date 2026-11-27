//! The restartable group kotlinc's `ComposableFunctionBodyTransformer` gives a skippable composable
//! function, and the composer arguments `ComposerParamTransformer` passes at its composable calls.
//!
//! A composable function gains a `$composer: Composer?` and a `$changed: Int` parameter, and its
//! body becomes
//!
//! ```text
//! $composer = $composer.startRestartGroup(key)
//! sourceInformation($composer, "C(A)N(text)…")
//! var $dirty = $changed
//! if ($changed and 0b0110 == 0) $dirty = $dirty or if ($composer.changed(text)) 0b0100 else 0b0010
//! if ($composer.shouldExecute($dirty and 0b0011 != 0b0010, $dirty and 1)) {
//!     if (isTraceInProgress()) traceEventStart(key, $dirty, -1, "pkg.A (A.kt:4)")
//!     … the source statements …
//!     if (isTraceInProgress()) traceEventEnd()
//! } else {
//!     $composer.skipToGroupEnd()
//! }
//! $composer.endRestartGroup()?.updateScope { $composer, $force ->
//!     A(text, $composer, updateChangedFlags($changed or 1))
//! }
//! ```
//!
//! `$changed` holds three bits per parameter after its low "force" bit: two for what the caller
//! knows of the argument (uncertain, the same as last time, different, or static) and one for its
//! stability. The function compares each parameter it reads whose state is uncertain and skips
//! only when every one it reads is the same or static; `$dirty` is that refined copy, and a call
//! passes on what it knows of each argument the same way. A function that reads no parameter has
//! no `$dirty` and decides on `$changed` alone.
//!
//! A function with constant default arguments also takes a `$default` mask of the arguments its
//! caller omitted. An omitted argument is static, and the group assigns its default before the
//! body: `if ($default and 0b10 != 0) count = 3`. Calls pass a placeholder for an omitted argument.
//!
//! A `@Composable () -> Unit` parameter becomes a `(Composer, Int) -> Unit` value, compared with
//! `changedInstance` and called with the composer and its own state bits.
//!
//! A composable lambda passed to one takes the `$composer` and `$changed` of that calling
//! convention, and its body becomes a group without a start or end of its own, since the runtime's
//! `ComposableLambdaImpl` opens and closes it:
//! `if ($composer.shouldExecute($changed and 0b0011 != 0b0010, $changed and 1)) { … } else
//! $composer.skipToGroupEnd()`.
//!
//! The IR is built in the shape common lowering gives the same statements written in source, so
//! every target realizes it exactly as it realizes them.

use crate::ir::{
    Callee, ExprId, FunId, IrBinOp, IrConst, IrEnclosure, IrExpr, IrFile, IrFunction,
    IrNodePosition, IrParameterIdentity, IrShortCircuitKind, IrTypeOp,
};
use crate::types::{EqualityMode, Ty, TyKind, TypeName};

use super::class_stability::{read_stable, ParameterStability};
use super::runtime::{self, ComposeRuntime};
use super::source_facts::{
    function_body, lambda_body, ArgumentState, ComposableCallSite, ComposableLambda, Memoization,
    RestartableFunction, TrackedParameter,
};

/// Bits per parameter slot of a `$changed` int, after its low "force" bit.
const BITS_PER_SLOT: u32 = 3;

/// The most value parameters one `$changed` int tracks; a function with more takes several.
pub(super) const SLOTS_PER_INT: usize = 10;

/// `ParamState`: what a caller knows of an argument, in a slot's two low bits.
const SAME: i32 = 0b001;
const DIFFERENT: i32 = 0b010;
const STATIC: i32 = 0b011;
/// The stability bit of a slot, set when the argument's type is unstable.
const STABILITY: i32 = 0b100;
/// A slot's two state bits and its stability bit.
const SLOT_MASK: i32 = 0b111;

/// `bitsForSlot`: `bits` moved into parameter slot `slot`.
fn bits_for_slot(bits: i32, slot: u32) -> i32 {
    bits << (1 + slot * BITS_PER_SLOT)
}

/// The value slots of a transformed static function's generated values: its two parameters after
/// the source ones, then `$dirty` when the body reads a parameter.
#[derive(Clone, Copy)]
struct Slots {
    /// The value index of the first parameter the group compares: a function's first parameter, or
    /// the first value its lambda is called with, after the values it captures.
    first: u32,
    composer: u32,
    /// The first `$changed`; the `changed_ints` of them follow one another.
    changed: u32,
    changed_ints: u32,
    /// `$default`, when a parameter has a default value.
    default: Option<u32>,
    /// The first `$dirty`, when the body reads a parameter: every `$changed` int has one then, and
    /// they follow one another in the order of their ints.
    dirty: Option<u32>,
}

impl Slots {
    /// A composable lambda's, after the values it captures and takes: it decides on `$changed`
    /// alone.
    fn lambda(lambda: &ComposableLambda) -> Self {
        let leading = lambda.captures + lambda.parameters.len() as u32;
        Self {
            first: lambda.captures,
            composer: leading,
            changed: leading + 1,
            changed_ints: 1,
            default: None,
            dirty: lambda
                .parameters
                .iter()
                .any(|parameter| parameter.used)
                .then_some(leading + 2),
        }
    }

    fn of(facts: &RestartableFunction) -> Self {
        let parameters = facts.parameters.len() as u32;
        let changed_ints = changed_ints(facts.parameters.len());
        let default = has_defaults(facts).then_some(parameters + 1 + changed_ints);
        let next = parameters + 1 + changed_ints + u32::from(default.is_some());
        Self {
            first: 0,
            composer: parameters,
            changed: parameters + 1,
            changed_ints,
            default,
            dirty: facts
                .parameters
                .iter()
                .any(|parameter| parameter.used)
                .then_some(next),
        }
    }

    /// The `$changed` holding parameter slots `int * 10` on.
    fn changed_int(self, int: u32) -> u32 {
        self.changed + int
    }

    /// The `$dirty` of `$changed` int `int`.
    fn dirty_int(self, int: u32) -> Option<u32> {
        self.dirty.map(|dirty| dirty + int)
    }

    /// The bit set the body decides int `int` on: its `$dirty` when there is one.
    fn decided(self, int: u32) -> u32 {
        self.dirty_int(int).unwrap_or_else(|| self.changed_int(int))
    }

    /// The values the generated ones move the body's locals past.
    fn generated(self) -> u32 {
        1 + self.changed_ints
            + u32::from(self.default.is_some())
            + self.dirty.map_or(0, |_| self.changed_ints)
    }
}

/// How many `$changed` ints a function of `parameters` value parameters takes.
pub(super) fn changed_ints(parameters: usize) -> u32 {
    parameters.div_ceil(SLOTS_PER_INT).max(1) as u32
}

/// The name of `$changed` or `$dirty` int `int`: kotlinc numbers every one but the first.
fn int_name(base: &str, int: u32) -> String {
    match int {
        0 => base.to_string(),
        _ => format!("{base}{int}"),
    }
}

fn has_defaults(facts: &RestartableFunction) -> bool {
    facts
        .parameters
        .iter()
        .any(|parameter| parameter.default.is_some())
}

/// What a call needs of its callee: the parameter types it passes a placeholder of for an omitted
/// argument, and whether the callee takes a `$default` mask.
struct CalleeShape {
    parameters: Vec<Ty>,
    defaults: bool,
}

/// `$default`'s bit for the parameter at `index`.
fn default_bit(index: u32) -> i32 {
    1 << index
}

pub(super) fn transform(ir: &mut IrFile) {
    let Some(facts) = ir.compose.take() else {
        return;
    };
    let callees = facts
        .functions
        .iter()
        .map(|function| {
            let shape = CalleeShape {
                parameters: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty)
                    .collect(),
                defaults: has_defaults(function),
            };
            (function.function, shape)
        })
        .collect::<crate::fx_hash::HashMap<_, _>>();
    for function in &facts.functions {
        add_composer_parameters(ir, function);
    }
    for taker in &facts.lambda_takers {
        // An extension receiver precedes the declared parameters in the physical ones.
        let receiver = usize::from(ir.extension_receiver_fns.contains(&taker.function));
        let declaration = &mut ir.functions[taker.function as usize];
        for &(index, _) in &taker.parameters {
            if let TyKind::Fun(signature) = declaration.params[index + receiver].kind() {
                declaration.params[index + receiver] =
                    runtime::composable_function(&signature.params);
            }
        }
        ir.plugin_extended_signatures.insert(taker.function);
        if let Some(scheme) = &taker.inferred_target {
            ir.function_annotations
                .entry(taker.function)
                .or_default()
                .push_metadata_visible(runtime::composable_inferred_target(scheme));
        }
    }
    // A lambda's calls are given their composer before its callers read it from the singletons
    // or remember it.
    for lambda in facts
        .lambdas
        .iter()
        .filter(|lambda| !matches!(lambda.memoization, Memoization::Inline { .. }))
    {
        add_lambda_composer_parameters(ir, lambda);
        let slots = Slots::lambda(lambda);
        for inner in &facts.lambdas {
            if inner.memoization
                == (Memoization::Remembered {
                    caller: lambda.implementation,
                })
            {
                remember(ir, &facts.runtime, inner, slots);
            }
        }
        for site in &lambda.calls {
            pass_composer(ir, slots, &callees, site);
        }
        lambda_group(ir, &facts.runtime, lambda);
        annotate_lambda(ir, lambda);
    }
    super::singleton_lambdas::memoize(ir, &facts.runtime, &facts.lambdas, facts.singletons);
    for lambda in &facts.lambdas {
        if matches!(lambda.memoization, Memoization::Instance { .. }) {
            instantiate(ir, &facts.runtime, lambda);
        }
    }
    for function in &facts.functions {
        let slots = Slots::of(function);
        for lambda in &facts.lambdas {
            if lambda.memoization
                == (Memoization::Remembered {
                    caller: function.function,
                })
            {
                remember(ir, &facts.runtime, lambda, slots);
            }
        }
        for lambda in &facts.lambdas {
            if lambda.memoization
                == (Memoization::Inline {
                    caller: function.function,
                })
            {
                place_lambda(ir, &facts.runtime, &callees, lambda, slots);
            }
        }
        for site in &function.calls {
            pass_composer(ir, slots, &callees, site);
        }
        if function.restartable {
            restart_group(ir, &facts.runtime, function);
        } else {
            traced_result(ir, &facts.runtime, function);
        }
        annotate_key(ir, function);
    }
}

/// Append `$composer` and `$changed` to a static function's source parameters, moving its locals
/// past them and `$dirty`, and record that its physical signature is no longer its declared one.
fn add_composer_parameters(ir: &mut IrFile, facts: &RestartableFunction) {
    let function = facts.function;
    let slots = Slots::of(facts);
    let source_parameters = facts.parameters.len();
    let declaration = &mut ir.functions[function as usize];
    let generated = 1 + slots.changed_ints as usize + usize::from(slots.default.is_some());
    for (index, parameter) in facts.parameters.iter().enumerate() {
        declaration.params[index] = physical_type(parameter);
    }
    declaration.params.push(Ty::nullable(runtime::composer()));
    declaration
        .params
        .extend((0..slots.changed_ints).map(|_| Ty::Int));
    declaration.params.extend(slots.default.map(|_| Ty::Int));
    declaration
        .param_checks
        .resize(source_parameters + generated, None);
    // `ComposerParamTransformer` makes a defaulted reference parameter nullable, since a call that
    // omits it passes `null` in its place: it is neither checked nor annotated `@NotNull`.
    for (index, parameter) in facts.parameters.iter().enumerate() {
        if parameter.default.is_some() && placeholder(parameter.ty) == IrConst::Null {
            declaration.param_checks[index] = None;
        }
    }
    let body = declaration.body.expect("a restartable function has a body");
    crate::ir::shift_value_indices(ir, body, slots.composer, slots.generated());
    let parameters = ir.fn_params.entry(function).or_default();
    parameters
        .identities
        .push(IrParameterIdentity::producer_value("$composer"));
    parameters.identities.extend(
        (0..slots.changed_ints)
            .map(|int| IrParameterIdentity::producer_value(int_name("$changed", int))),
    );
    if slots.default.is_some() {
        parameters.identities.push(IrParameterIdentity::generated(
            crate::ir::IrGeneratedParameterRole::DefaultMask,
            None,
        ));
        // The group assigns the defaults itself, so the function has no `$default` synthetic.
        parameters.defaults = None;
    }
    let nullable = &mut ir.declared_parameters_mut(function).nullable;
    nullable.resize(source_parameters, false);
    for (index, parameter) in facts.parameters.iter().enumerate() {
        if parameter.default.is_some() && placeholder(parameter.ty) == IrConst::Null {
            nullable[index] = true;
        }
    }
    nullable.push(true);
    nullable.extend((0..slots.changed_ints).map(|_| false));
    nullable.extend(slots.default.map(|_| false));
    ir.plugin_extended_signatures.insert(function);
}

/// Make the lambda an inline library composable places in its caller take the caller's `$composer`
/// and a `$changed` of its own, after the values it captures, and the caller's `$dirty` among them
/// when its calls pass the caller's parameters on. Its body runs between source-information
/// markers, not in a group:
///
/// ```text
/// sourceInformationMarkerStart($composer, key, "C4@201L10:file.kt#hash")
/// … the source statements …
/// sourceInformationMarkerEnd($composer)
/// ```
fn place_lambda(
    ir: &mut IrFile,
    runtime: &ComposeRuntime,
    callees: &crate::fx_hash::HashMap<FunId, CalleeShape>,
    lambda: &ComposableLambda,
    caller: Slots,
) {
    let template = lambda
        .inline_template
        .expect("a lambda an inline call places has its template");
    let forwards = lambda.calls.iter().any(|site| {
        site.arguments
            .iter()
            .any(|argument| matches!(argument, ArgumentState::Forwarded(_)))
    });
    let dirty = forwards.then(|| {
        caller
            .dirty
            .expect("a function that passes its parameters on reads them")
    });
    let captures = lambda.captures;
    let values = lambda.parameters.len() as u32;
    let extra = u32::from(dirty.is_some());
    let slots = Slots {
        first: captures + extra,
        composer: captures + extra + values,
        changed: captures + extra + values + 1,
        changed_ints: 1,
        default: None,
        dirty: dirty.map(|_| captures),
    };
    // The values the lambda captures, the caller's `$dirty` last, then the ones it is called with,
    // then the two the composer adds.
    let dirty_capture = dirty.map(|slot| ir.add_expr(IrExpr::GetValue(slot)));
    let function = lambda.implementation;
    let declaration = &mut ir.functions[function as usize];
    debug_assert_eq!(declaration.params.len(), (captures + values) as usize);
    if dirty.is_some() {
        declaration.params.insert(captures as usize, Ty::Int);
    }
    declaration
        .params
        .extend([Ty::nullable(runtime::composer()), Ty::Int]);
    declaration
        .param_checks
        .resize((captures + values + extra) as usize + 2, None);
    let body = declaration.body.expect("a lambda has a body");
    for expression in [body, template] {
        crate::ir::shift_value_indices(ir, expression, captures, extra);
        crate::ir::shift_value_indices(ir, expression, captures + extra + values, 2);
    }
    let identities = &mut ir.fn_params.entry(function).or_default().identities;
    if dirty.is_some() {
        identities.insert(
            captures as usize,
            IrParameterIdentity::producer_value("$dirty"),
        );
    }
    identities.extend([
        IrParameterIdentity::producer_value("$composer"),
        IrParameterIdentity::producer_value("$changed"),
    ]);
    let IrExpr::Lambda {
        arity,
        captures: captured,
        ..
    } = &mut ir.exprs[lambda.lambda as usize]
    else {
        unreachable!("a composable lambda is a lambda");
    };
    *arity = composer_arity(lambda.parameters.len());
    captured.extend(dirty_capture);
    // The standalone function is never emitted, and its library calls lack the composer.
    retire_library_calls(ir, body);
    for site in &lambda.calls {
        pass_composer(ir, slots, callees, site);
    }
    let IrExpr::Block { stmts, value } = ir.exprs[template as usize].clone() else {
        unreachable!("a placed lambda's template is a block");
    };
    let mut statements = Vec::with_capacity(stmts.len() + 2);
    if let Some(information) = &lambda.source_information {
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        let key = ir.add_expr(IrExpr::Const(IrConst::Int(lambda.key)));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(information.as_str().into())));
        statements.push(runtime::call(
            ir,
            &runtime.source_information_marker_start,
            None,
            vec![composer, key, text],
        ));
    }
    statements.extend(stmts);
    if lambda.source_information.is_some() {
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        statements.push(runtime::call(
            ir,
            &runtime.source_information_marker_end,
            None,
            vec![composer],
        ));
    }
    ir.exprs[template as usize] = IrExpr::Block {
        stmts: statements,
        value,
    };
}

/// Give the function a composable lambda is lowered to the `$composer` and `$changed` its value
/// is called with, after the values it captures, and move its locals past them.
fn add_lambda_composer_parameters(ir: &mut IrFile, lambda: &ComposableLambda) {
    let function = lambda.implementation;
    let leading = lambda.captures as usize + lambda.parameters.len();
    let declaration = &mut ir.functions[function as usize];
    debug_assert_eq!(
        declaration.params.len(),
        leading,
        "a composable lambda takes only its captures and its own parameters"
    );
    declaration.params.extend([runtime::composer(), Ty::Int]);
    declaration.param_checks.resize(leading + 2, None);
    let body = declaration.body.expect("a lambda has a body");
    crate::ir::shift_value_indices(ir, body, leading as u32, Slots::lambda(lambda).generated());
    let identities = &mut ir.fn_params.entry(function).or_default().identities;
    identities.truncate(leading);
    identities.extend([
        IrParameterIdentity::producer_value("$composer"),
        IrParameterIdentity::producer_value("$changed"),
    ]);
    let IrExpr::Lambda {
        arity, inline_body, ..
    } = &mut ir.exprs[lambda.lambda as usize]
    else {
        unreachable!("a composable lambda is a lambda");
    };
    *arity = composer_arity(lambda.parameters.len());
    // The value is called through the runtime's wrapper, never inlined.
    let template = inline_body.take();
    if let Some(template) = template {
        retire_library_calls(ir, template);
    }
    // Another expression for the same implementation, a copy the lowering made of the body that
    // wrote it, carries a form of the lambda too.
    let copies = (0..ir.exprs.len() as ExprId)
        .filter(|&copy| {
            matches!(
                ir.exprs[copy as usize],
                IrExpr::Lambda { impl_fn, inline_body: Some(_), .. } if impl_fn == function
            )
        })
        .collect::<Vec<_>>();
    for copy in copies {
        if let IrExpr::Lambda { inline_body, .. } = &mut ir.exprs[copy as usize] {
            if let Some(template) = inline_body.take() {
                retire_library_calls(ir, template);
            }
        }
    }
}

/// Empty the library composable calls of a lambda's dropped inline form. Nothing emits that form,
/// and a library composable call there would still lack the composer arguments its compiled
/// method takes.
fn retire_library_calls(ir: &mut IrFile, template: ExprId) {
    let mut pending = vec![template];
    let mut library = Vec::new();
    while let Some(expression) = pending.pop() {
        if let IrExpr::Call { callee, .. } = &ir.exprs[expression as usize] {
            if matches!(**callee, Callee::External { .. }) {
                library.push(expression);
                continue;
            }
        }
        // A lambda written in the form carries a form of its own.
        if let IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } = ir.exprs[expression as usize]
        {
            pending.push(body);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    for call in library {
        ir.exprs[call as usize] = IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        };
    }
}

/// The body kotlinc gives a composable lambda. The runtime's `ComposableLambda` wrapper starts
/// its group, so the body only decides, on `$changed` alone, whether to run:
///
/// ```text
/// sourceInformation($composer, "C…")
/// if ($composer.shouldExecute($changed and 0b011 != 0b010, $changed and 1)) {
///     … the source statements, traced …
/// } else {
///     $composer.skipToGroupEnd()
/// }
/// ```
fn lambda_group(ir: &mut IrFile, runtime: &ComposeRuntime, lambda: &ComposableLambda) {
    let body =
        lambda_body(ir, lambda.implementation).expect("a composable lambda has a block body");
    let slots = Slots::lambda(lambda);
    let mut statements = Vec::new();
    if let Some(information) = &lambda.source_information {
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(information.as_str().into())));
        statements.push(runtime::call(
            ir,
            &runtime.source_information,
            None,
            vec![composer, text],
        ));
    }
    let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
    dirty_preamble(ir, runtime, &lambda.parameters, 0, slots, &mut statements);
    let read = lambda_read_bits(lambda);
    let state = and_constant(ir, slots.decided(0), 1 | read);
    let any_changed = int_comparison(ir, IrBinOp::Ne, state, read);
    let force = and_constant(ir, slots.decided(0), 1);
    let execute = runtime::call(
        ir,
        &runtime.should_execute,
        Some(composer),
        vec![any_changed, force],
    );
    let executed = traced(
        ir,
        runtime,
        lambda.key,
        slots,
        lambda.trace.as_deref(),
        Vec::new(),
        &body.statements,
    );
    let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
    let skip = runtime::call(ir, &runtime.skip_to_group_end, Some(composer), Vec::new());
    // kotlinc skips at the lambda's start offset, its opening brace.
    mark_line(ir, skip, lambda.opening_line);
    let skipped = ir.add_expr(IrExpr::Block {
        stmts: vec![skip],
        value: None,
    });
    let branch = ir.add_expr(IrExpr::When {
        branches: vec![(Some(execute), executed), (None, skipped)],
    });
    ir.whens.exhaustive.insert(branch, Ty::Unit);
    statements.push(branch);
    ir.exprs[body.block as usize] = IrExpr::Block {
        stmts: statements,
        value: None,
    };
    // kotlinc builds the group without offsets: the first line is its first statement's.
    ir.take_statement_line(body.block);
    let root = ir.functions[lambda.implementation as usize]
        .body
        .expect("a lambda has a body");
    ir.mark_positionless(root);
}

/// `@Composable`, and `@FunctionKeyMeta` when the option asks for it, on the lambda's function.
fn annotate_lambda(ir: &mut IrFile, lambda: &ComposableLambda) {
    let annotations = ir
        .function_annotations
        .entry(lambda.implementation)
        .or_default();
    annotations.push(runtime::composable());
    if lambda.key_meta {
        annotations.push(runtime::function_key_meta(lambda.key, lambda.offsets));
    }
}

/// Pass `rememberComposableLambda(key, true, lambda, $composer, 0b110110)` in a capturing
/// lambda's place: the runtime keeps one wrapper per call in the composition of the caller,
/// whose `slots` give its composer, and updates the block the wrapper calls. Both constants are
/// static, which the `$changed` argument says.
fn remember(ir: &mut IrFile, runtime: &ComposeRuntime, lambda: &ComposableLambda, slots: Slots) {
    let key = ir.add_expr(IrExpr::Const(IrConst::Int(lambda.key)));
    let tracked = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
    let changed = ir.add_expr(IrExpr::Const(IrConst::Int(
        bits_for_slot(STATIC, 0) | bits_for_slot(STATIC, 1),
    )));
    for argument in [tracked, composer, changed] {
        ir.mark_positionless(argument);
    }
    let remembered = runtime::call(
        ir,
        &runtime.remember_composable_lambda,
        None,
        vec![key, tracked, lambda.lambda, composer, changed],
    );
    pass_wrapped(ir, lambda, key, remembered);
}

/// Pass `composableLambdaInstance(key, true, lambda)` in a capturing lambda's place, where a
/// function that is not composable passes it: no composition keeps the wrapper, so each call
/// makes one.
fn instantiate(ir: &mut IrFile, runtime: &ComposeRuntime, lambda: &ComposableLambda) {
    let key = ir.add_expr(IrExpr::Const(IrConst::Int(lambda.key)));
    let tracked = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    ir.mark_positionless(tracked);
    let created = runtime::call(
        ir,
        &runtime.composable_lambda_instance,
        None,
        vec![key, tracked, lambda.lambda],
    );
    pass_wrapped(ir, lambda, key, created);
}

/// Replace `lambda` in its call's arguments by `wrapper`, cast to the composable calling
/// convention. `key` is the wrapper's first argument.
fn pass_wrapped(ir: &mut IrFile, lambda: &ComposableLambda, key: ExprId, wrapper: ExprId) {
    let value = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: wrapper,
        type_operand: runtime::composable_function(&lambda.parameter_types()),
    });
    // kotlinc builds the call at the lambda's offsets, so the lambda's line begins at its key.
    if lambda.opening_line != 0 {
        for expression in [key, wrapper, value] {
            ir.set_source_line(expression, lambda.opening_line);
        }
    }
    let IrExpr::Call { args, .. } = &mut ir.exprs[lambda.call as usize] else {
        unreachable!("a composable lambda is a call's argument");
    };
    let argument = args
        .iter_mut()
        .find(|argument| **argument == lambda.lambda)
        .expect("the call passes the lambda");
    *argument = value;
}

/// The statements a group runs when it executes: `executed`, then `statements` between the trace
/// events when trace markers are enabled.
fn traced(
    ir: &mut IrFile,
    runtime: &ComposeRuntime,
    key: i32,
    slots: Slots,
    trace: Option<&str>,
    mut executed: Vec<ExprId>,
    statements: &[ExprId],
) -> ExprId {
    if let Some(trace) = trace {
        let key = ir.add_expr(IrExpr::Const(IrConst::Int(key)));
        let changed = ir.add_expr(IrExpr::GetValue(slots.decided(0)));
        // The event records the states of the first two ints, and `-1` where there is no second.
        let unused = if slots.changed_ints > 1 {
            ir.add_expr(IrExpr::GetValue(slots.decided(1)))
        } else {
            ir.add_expr(IrExpr::Const(IrConst::Int(-1)))
        };
        let text = ir.add_expr(IrExpr::Const(IrConst::String(trace.into())));
        let event = runtime::call(
            ir,
            &runtime.trace_event_start,
            None,
            vec![key, changed, unused, text],
        );
        executed.push(if_trace_in_progress(ir, runtime, event));
    }
    executed.extend(statements.iter().copied());
    if trace.is_some() {
        let event = runtime::call(ir, &runtime.trace_event_end, None, Vec::new());
        executed.push(if_trace_in_progress(ir, runtime, event));
    }
    ir.add_expr(IrExpr::Block {
        stmts: executed,
        value: None,
    })
}

/// A parameter's type after `ComposerParamTransformer`, which gives a composable function value the
/// calling convention too.
fn physical_type(parameter: &TrackedParameter) -> Ty {
    match (parameter.lambda, parameter.ty.kind()) {
        (Some(_), TyKind::Fun(signature)) => runtime::composable_function(&signature.params),
        _ => parameter.ty,
    }
}

/// Give a composable call the caller's composer and what the caller knows of each argument, and,
/// for a callee with defaults, a placeholder for each omitted argument and the mask naming them.
fn pass_composer(
    ir: &mut IrFile,
    caller: Slots,
    callees: &crate::fx_hash::HashMap<FunId, CalleeShape>,
    site: &ComposableCallSite,
) {
    let ComposableCallSite {
        call,
        ref arguments,
        library_mask,
        receiver,
        ref stability,
    } = *site;
    let arguments = arguments.as_slice();
    let stability = stability.as_slice();
    if let IrExpr::InvokeFunction { .. } = ir.exprs[call as usize] {
        let composer = ir.add_expr(IrExpr::GetValue(caller.composer));
        let changed = changed_arguments(ir, caller, arguments, stability);
        ir.mark_positionless(composer);
        for &int in &changed {
            ir.mark_positionless(int);
        }
        let IrExpr::InvokeFunction { args, params, .. } = &mut ir.exprs[call as usize] else {
            unreachable!("an invoke is still an invoke");
        };
        args.push(composer);
        args.extend(&changed);
        params.push(runtime::composer());
        params.extend(changed.iter().map(|_| Ty::Int));
        return;
    }
    let IrExpr::Call { callee, args, .. } = &ir.exprs[call as usize] else {
        unreachable!("a composable call is a call");
    };
    // A library composable takes the composer parameters already, after every declared one, and
    // the mask of the omitted ones when it declares defaults.
    if let Callee::External { params, .. } = &**callee {
        let params = params.clone();
        let mut supplied = args.clone().into_iter();
        let mut physical = Vec::with_capacity(params.len() + 3);
        let mut omitted = 0;
        let declared = &arguments[usize::from(receiver)..];
        for ((index, argument), &ty) in (0u32..).zip(declared).zip(&params) {
            if *argument == ArgumentState::Omitted {
                omitted |= default_bit(index);
                let placeholder = ir.add_expr(IrExpr::Const(placeholder(ty)));
                ir.mark_positionless(placeholder);
                physical.push(placeholder);
            } else {
                physical.push(
                    supplied
                        .next()
                        .expect("a supplied argument per provided parameter"),
                );
            }
        }
        let composer = ir.add_expr(IrExpr::GetValue(caller.composer));
        let changed = changed_arguments(ir, caller, arguments, stability);
        ir.mark_positionless(composer);
        for &int in &changed {
            ir.mark_positionless(int);
        }
        physical.push(composer);
        physical.extend(changed);
        if library_mask {
            let mask = ir.add_expr(IrExpr::Const(IrConst::Int(omitted)));
            ir.mark_positionless(mask);
            physical.push(mask);
        }
        let IrExpr::Call { callee, args, .. } = &mut ir.exprs[call as usize] else {
            unreachable!("a composable call is a call");
        };
        // The callee fills its own defaults now, so the call passes every parameter.
        if let Callee::External { defaults, .. } = &mut **callee {
            defaults.clear();
        }
        *args = physical;
        return;
    }
    // Another file's composable takes them too, once that file's own pass gives it them: the call
    // names the physical signature that pass produces.
    if matches!(
        **callee,
        Callee::Module { .. }
            | Callee::CrossFile {
                module_target: Some(_),
                ..
            }
    ) {
        let composer = ir.add_expr(IrExpr::GetValue(caller.composer));
        let changed = changed_argument(ir, caller, arguments, stability);
        ir.mark_positionless(composer);
        ir.mark_positionless(changed);
        let IrExpr::Call { callee, args, .. } = &mut ir.exprs[call as usize] else {
            unreachable!("a composable call is a call");
        };
        let (Callee::Module { params, .. } | Callee::CrossFile { params, .. }) = &mut **callee
        else {
            unreachable!("a module call is still a module call");
        };
        params.extend([runtime::composer(), Ty::Int]);
        args.extend([composer, changed]);
        return;
    }
    let function = callee
        .source_function()
        .expect("a composable call in the file calls one of its functions");
    let shape = &callees[&function];
    let mut supplied = args.clone().into_iter();
    let mut physical = Vec::with_capacity(arguments.len() + 3);
    let mut omitted = 0;
    for ((index, argument), &ty) in (0u32..).zip(arguments).zip(&shape.parameters) {
        if *argument == ArgumentState::Omitted {
            omitted |= default_bit(index - u32::from(receiver));
            let placeholder = ir.add_expr(IrExpr::Const(placeholder(ty)));
            ir.mark_positionless(placeholder);
            physical.push(placeholder);
        } else {
            physical.push(
                supplied
                    .next()
                    .expect("a supplied argument per provided parameter"),
            );
        }
    }
    let composer = ir.add_expr(IrExpr::GetValue(caller.composer));
    let changed = changed_arguments(ir, caller, arguments, stability);
    // kotlinc builds these arguments without offsets, so the call's line begins at its dispatch.
    ir.mark_positionless(composer);
    for &int in &changed {
        ir.mark_positionless(int);
    }
    physical.push(composer);
    physical.extend(changed);
    if shape.defaults {
        let mask = ir.add_expr(IrExpr::Const(IrConst::Int(omitted)));
        ir.mark_positionless(mask);
        physical.push(mask);
    }
    let IrExpr::Call { callee, args, .. } = &mut ir.exprs[call as usize] else {
        unreachable!("a composable call is a call");
    };
    // The callee fills its own defaults now, so the call passes every parameter.
    **callee = Callee::Local(function);
    *args = physical;
}

/// The value kotlinc passes for an omitted argument of type `ty`: the type's zero, or `null`.
fn placeholder(ty: Ty) -> IrConst {
    match ty {
        Ty::Boolean => IrConst::Boolean(false),
        Ty::Char => IrConst::Char(0),
        Ty::Byte => IrConst::Byte(0),
        Ty::Short => IrConst::Short(0),
        Ty::Int => IrConst::Int(0),
        Ty::Float => IrConst::Float(0.0),
        Ty::Long => IrConst::Long(0),
        Ty::Double => IrConst::Double(0.0),
        _ => IrConst::Null,
    }
}

/// `buildChangedArgumentForCall`: the `$changed` ints of a call, one for each ten arguments.
fn changed_arguments(
    ir: &mut IrFile,
    caller: Slots,
    arguments: &[ArgumentState],
    stability: &[Vec<TypeName>],
) -> Vec<ExprId> {
    (0..changed_ints(arguments.len()))
        .map(|int| {
            let first = (int as usize * SLOTS_PER_INT).min(arguments.len());
            let covered = &arguments[first..];
            let covered = &covered[..covered.len().min(SLOTS_PER_INT)];
            let stability = &stability[first.min(stability.len())..];
            changed_argument(ir, caller, covered, stability)
        })
        .collect()
}

/// A constant holding each static argument's state, or'ed with the caller's own `$dirty` bits for
/// each parameter it passes on, moved to the callee's slot, for the arguments of one `$changed`
/// int. An argument of a type whose stability the callee reads at run time carries the `$stable` of
/// its class, moved to the argument's slot.
fn changed_argument(
    ir: &mut IrFile,
    caller: Slots,
    arguments: &[ArgumentState],
    stability: &[Vec<TypeName>],
) -> ExprId {
    let mut constant = 0;
    let mut forwarded = Vec::new();
    for (slot, argument) in (0u32..).zip(arguments) {
        match *argument {
            ArgumentState::Static => constant |= bits_for_slot(STATIC, slot),
            ArgumentState::Uncertain | ArgumentState::Omitted => {}
            ArgumentState::Forwarded(parent) => {
                let dirty = caller
                    .dirty_int(parent / SLOTS_PER_INT as u32)
                    .expect("a caller that passes on a parameter reads it");
                let parent = parent % SLOTS_PER_INT as u32;
                let mask = ir.add_expr(IrExpr::Const(IrConst::Int(bits_for_slot(SLOT_MASK, slot))));
                let bits = ir.add_expr(IrExpr::GetValue(dirty));
                let shifted = match slot.cmp(&parent) {
                    std::cmp::Ordering::Equal => bits,
                    order => {
                        let distance = slot.abs_diff(parent) * BITS_PER_SLOT;
                        let distance = ir.add_expr(IrExpr::Const(IrConst::Int(distance as i32)));
                        let shift = if order.is_gt() {
                            IrBinOp::Shl
                        } else {
                            IrBinOp::Shr
                        };
                        ir.add_arithmetic(shift, bits, distance, Ty::Int)
                    }
                };
                let masked = ir.add_arithmetic(IrBinOp::BitAnd, mask, shifted, Ty::Int);
                let reads = stability.get(slot as usize).map_or(&[][..], Vec::as_slice);
                let mut stable = None;
                for &class in reads {
                    let mut read = read_stable(ir, class);
                    if slot != 0 {
                        let distance = (slot * BITS_PER_SLOT) as i32;
                        let distance = ir.add_expr(IrExpr::Const(IrConst::Int(distance)));
                        read = ir.add_arithmetic(IrBinOp::Shl, read, distance, Ty::Int);
                    }
                    stable = Some(match stable {
                        None => read,
                        Some(combined) => or(ir, combined, read),
                    });
                }
                let bits = match stable {
                    None => masked,
                    Some(stable) => or(ir, stable, masked),
                };
                forwarded.push(bits);
            }
        }
    }
    let mut operands = forwarded.into_iter();
    let first = match (constant, operands.next()) {
        (_, None) => return ir.add_expr(IrExpr::Const(IrConst::Int(constant))),
        (0, Some(first)) => first,
        (_, Some(first)) => {
            let constant = ir.add_expr(IrExpr::Const(IrConst::Int(constant)));
            or(ir, constant, first)
        }
    };
    operands.fold(first, |combined, next| or(ir, combined, next))
}

fn or(ir: &mut IrFile, lhs: ExprId, rhs: ExprId) -> ExprId {
    ir.add_arithmetic(IrBinOp::BitOr, lhs, rhs, Ty::Int)
}

fn and_constant(ir: &mut IrFile, value: u32, bits: i32) -> ExprId {
    let lhs = ir.add_expr(IrExpr::GetValue(value));
    let rhs = ir.add_expr(IrExpr::Const(IrConst::Int(bits)));
    ir.add_arithmetic(IrBinOp::BitAnd, lhs, rhs, Ty::Int)
}

fn int_comparison(ir: &mut IrFile, op: IrBinOp, lhs: ExprId, rhs: i32) -> ExprId {
    let rhs = ir.add_expr(IrExpr::Const(IrConst::Int(rhs)));
    ir.add_expr(IrExpr::Equality {
        op,
        mode: EqualityMode::Primitive,
        lhs,
        rhs,
    })
}

/// The states a lambda's group needs to see unchanged to skip: those of the parameters its body
/// reads and, in the slot after its parameters, the lambda's own, which the runtime's wrapper adds.
fn lambda_read_bits(lambda: &ComposableLambda) -> i32 {
    (0u32..)
        .zip(&lambda.parameters)
        .filter(|(_, parameter)| parameter.used)
        .fold(0, |mask, (slot, _)| mask | bits_for_slot(SAME, slot))
        | bits_for_slot(SAME, lambda.parameters.len() as u32)
}

/// `var $dirty = $changed`, then for each parameter the body reads, a comparison when the caller
/// left its state uncertain:
/// `if ($changed and 0b0110 == 0) $dirty = $dirty or if ($composer.changed(p)) 0b0100 else 0b0010`.
fn dirty_preamble(
    ir: &mut IrFile,
    runtime: &ComposeRuntime,
    parameters: &[TrackedParameter],
    receiver: u32,
    slots: Slots,
    statements: &mut Vec<ExprId>,
) {
    for int in 0..slots.changed_ints {
        let Some(dirty) = slots.dirty_int(int) else {
            continue;
        };
        let changed = ir.add_expr(IrExpr::GetValue(slots.changed_int(int)));
        let declaration = ir.add_expr(IrExpr::Variable {
            index: dirty,
            ty: Ty::Int,
            init: Some(changed),
            named: true,
        });
        ir.value_names.insert(declaration, int_name("$dirty", int));
        statements.push(declaration);
    }
    for (index, parameter) in (0u32..).zip(parameters) {
        if !parameter.used {
            continue;
        }
        let int = index / SLOTS_PER_INT as u32;
        let slot = index % SLOTS_PER_INT as u32;
        let dirty = slots
            .dirty_int(int)
            .expect("a body that reads a parameter has a `$dirty` for each int");
        let isolated = and_constant(ir, slots.changed_int(int), bits_for_slot(STATIC, slot));
        let uncertain = int_comparison(ir, IrBinOp::Eq, isolated, 0);
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        let value = ir.add_expr(IrExpr::GetValue(slots.first + index));
        let compared = match parameter.stability {
            ParameterStability::Stable => {
                // A function value is compared by identity too, whatever its stability.
                let changed = if parameter.lambda.is_some()
                    || matches!(parameter.ty.non_null().kind(), TyKind::Fun(_))
                {
                    &runtime.changed_instance
                } else {
                    runtime.changed_for(parameter.ty)
                };
                runtime::call(ir, changed, Some(composer), vec![value])
            }
            ParameterStability::Unstable => {
                runtime::call(ir, &runtime.changed_instance, Some(composer), vec![value])
            }
            // The caller's stability bit for the slot says, when the function runs, which
            // comparison applies: by value when it is clear, by identity when it is set.
            ParameterStability::Undecided => {
                let stable =
                    and_constant(ir, slots.changed_int(int), bits_for_slot(STABILITY, slot));
                let by_value = int_comparison(ir, IrBinOp::Eq, stable, 0);
                let by_value_call = runtime::call(
                    ir,
                    runtime.changed_for(parameter.ty),
                    Some(composer),
                    vec![value],
                );
                let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
                let value = ir.add_expr(IrExpr::GetValue(slots.first + index));
                let by_identity =
                    runtime::call(ir, &runtime.changed_instance, Some(composer), vec![value]);
                let when = ir.add_expr(IrExpr::When {
                    branches: vec![(Some(by_value), by_value_call), (None, by_identity)],
                });
                ir.whens.exhaustive.insert(when, Ty::Boolean);
                when
            }
        };
        let different = ir.add_expr(IrExpr::Const(IrConst::Int(bits_for_slot(DIFFERENT, slot))));
        let same = ir.add_expr(IrExpr::Const(IrConst::Int(bits_for_slot(SAME, slot))));
        let state = ir.add_expr(IrExpr::When {
            branches: vec![(Some(compared), different), (None, same)],
        });
        ir.whens.exhaustive.insert(state, Ty::Int);
        let update = or_into(ir, dirty, state);
        let compare = match (parameter.default, slots.default) {
            // A constant default is static: an omitted argument needs no comparison.
            (Some(_), Some(default)) => {
                let omitted = is_omitted(ir, default, index - receiver);
                let static_bits =
                    ir.add_expr(IrExpr::Const(IrConst::Int(bits_for_slot(STATIC, slot))));
                let defaulted = or_into(ir, dirty, static_bits);
                let otherwise = ir.add_expr(IrExpr::Block {
                    stmts: Vec::new(),
                    value: None,
                });
                let when = ir.add_expr(IrExpr::When {
                    branches: vec![
                        (Some(omitted), defaulted),
                        (Some(uncertain), update),
                        (None, otherwise),
                    ],
                });
                ir.whens.exhaustive.insert(when, Ty::Unit);
                when
            }
            _ => if_then(ir, uncertain, update),
        };
        statements.push(compare);
    }
}

/// `$dirty = $dirty or bits`
fn or_into(ir: &mut IrFile, dirty: u32, bits: ExprId) -> ExprId {
    let current = ir.add_expr(IrExpr::GetValue(dirty));
    let refined = or(ir, current, bits);
    ir.add_expr(IrExpr::SetValue {
        var: dirty,
        value: refined,
    })
}

/// `$default and bit != 0`: whether the caller omitted the argument at `index`.
fn is_omitted(ir: &mut IrFile, default: u32, index: u32) -> ExprId {
    let bit = and_constant(ir, default, default_bit(index));
    int_comparison(ir, IrBinOp::Ne, bit, 0)
}

/// `if ($default and bit != 0) p = <default>` for each parameter with a default value, which the
/// group runs before the body.
fn assign_defaults(ir: &mut IrFile, facts: &RestartableFunction, slots: Slots) -> Vec<ExprId> {
    let receiver = u32::from(facts.receiver);
    let Some(default) = slots.default else {
        return Vec::new();
    };
    (0u32..)
        .zip(&facts.parameters)
        .filter_map(|(index, parameter)| Some((index, parameter.default?)))
        .map(|(index, value)| {
            let omitted = is_omitted(ir, default, index - receiver);
            let assign = ir.add_expr(IrExpr::SetValue { var: index, value });
            if_then(ir, omitted, assign)
        })
        .collect()
}

/// `if (condition) statement`
fn if_then(ir: &mut IrFile, condition: ExprId, statement: ExprId) -> ExprId {
    let then = ir.add_expr(IrExpr::Block {
        stmts: vec![statement],
        value: None,
    });
    let otherwise = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let when = ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), then), (None, otherwise)],
    });
    ir.whens.exhaustive.insert(when, Ty::Unit);
    when
}

/// `irHasDifferences`: whether the group must run, from the force bit and the state of every
/// parameter the body reads. Strong skipping compares only each slot's low state bit, which is set
/// exactly when the argument is the same as last time or static.
fn has_differences(ir: &mut IrFile, facts: &RestartableFunction, slots: Slots) -> ExprId {
    if facts.parameters.is_empty() {
        let changed = ir.add_expr(IrExpr::GetValue(slots.changed));
        return int_comparison(ir, IrBinOp::Ne, changed, 0);
    }
    let mut differences = Vec::new();
    for (int, parameters) in (0u32..).zip(facts.parameters.chunks(SLOTS_PER_INT)) {
        let read = (0u32..)
            .zip(parameters)
            .filter(|(_, parameter)| parameter.used)
            .fold(0, |mask, (slot, _)| mask | bits_for_slot(SAME, slot));
        let decided = and_constant(ir, slots.decided(int), read | 1);
        differences.push(int_comparison(ir, IrBinOp::Ne, decided, read));
    }
    let mut differences = differences.into_iter();
    let first = differences
        .next()
        .expect("the first int always has a difference test");
    differences.fold(first, |left, right| short_circuit_or(ir, left, right))
}

/// `left || right`
fn short_circuit_or(ir: &mut IrFile, left: ExprId, right: ExprId) -> ExprId {
    let yes = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
    let or = ir.add_expr(IrExpr::When {
        branches: vec![(Some(left), yes), (None, right)],
    });
    ir.whens.exhaustive.insert(or, Ty::Boolean);
    ir.short_circuits.insert(or, IrShortCircuitKind::Or);
    or
}

fn restart_group(ir: &mut IrFile, runtime: &ComposeRuntime, facts: &RestartableFunction) {
    let body = function_body(ir, facts.function).expect("a restartable function has a block body");
    let slots = Slots::of(facts);
    let function_line = facts.opening_line;
    let mut statements = Vec::new();

    let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
    let key = ir.add_expr(IrExpr::Const(IrConst::Int(facts.key)));
    let started = runtime::call(ir, &runtime.start_restart_group, Some(composer), vec![key]);
    let start = ir.add_expr(IrExpr::SetValue {
        var: slots.composer,
        value: started,
    });
    mark_line(ir, start, function_line);
    statements.push(start);

    if let Some(information) = &facts.source_information {
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(information.as_str().into())));
        statements.push(runtime::call(
            ir,
            &runtime.source_information,
            None,
            vec![composer, text],
        ));
    }
    dirty_preamble(
        ir,
        runtime,
        &facts.parameters,
        u32::from(facts.receiver),
        slots,
        &mut statements,
    );

    // `$composer.shouldExecute(<differences>, $dirty and 1)`
    let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
    let any_changed = has_differences(ir, facts, slots);
    let force = and_constant(ir, slots.decided(0), 1);
    let execute = runtime::call(
        ir,
        &runtime.should_execute,
        Some(composer),
        vec![any_changed, force],
    );

    let defaults = assign_defaults(ir, facts, slots);
    let executed = traced(
        ir,
        runtime,
        facts.key,
        slots,
        facts.trace.as_deref(),
        defaults,
        &body.statements,
    );
    let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
    let skip = runtime::call(ir, &runtime.skip_to_group_end, Some(composer), Vec::new());
    // kotlinc skips at the declaration's start offset, the `fun` keyword.
    mark_line(ir, skip, facts.declaration_line);
    ir.set_source_line(skip, facts.declaration_line);
    let skipped = ir.add_expr(IrExpr::Block {
        stmts: vec![skip],
        value: None,
    });
    let branch = ir.add_expr(IrExpr::When {
        branches: vec![(Some(execute), executed), (None, skipped)],
    });
    ir.whens.exhaustive.insert(branch, Ty::Unit);
    // kotlinc's plugin builds the test and the code around the body without offsets: only the
    // branch itself begins on the function's line, and the body keeps its own positions.
    for built in [execute, executed] {
        ir.settle_tree_positions(built, IrNodePosition::default());
    }
    mark_line(ir, branch, function_line);
    statements.push(branch);

    let end = update_scope(ir, runtime, facts);
    mark_line(ir, end, facts.closing_line);
    statements.push(end);

    ir.exprs[body.block as usize] = IrExpr::Block {
        stmts: statements,
        value: None,
    };
}

/// The body of a composable function with a result and no group: its source statements between
/// the trace markers, with the value of the closing `return` held across the end marker.
///
/// ```text
/// if (isTraceInProgress()) traceEventStart(key, $changed, -1, "pkg.F (F.kt:3)")
/// … the source statements …
/// val tmp = <value>
/// if (isTraceInProgress()) traceEventEnd()
/// return tmp
/// ```
///
/// A constant or a value read is returned without the temporary. When the plugin writes source
/// information, a marker pair brackets the whole body in place of a group's `sourceInformation`.
fn traced_result(ir: &mut IrFile, runtime: &ComposeRuntime, facts: &RestartableFunction) {
    let body = function_body(ir, facts.function)
        .or_else(|| super::source_facts::expression_body(ir, facts.function))
        .expect("a result function has a block or an expression body");
    let slots = Slots::of(facts);
    let (&last, leading) = body
        .statements
        .split_last()
        .expect("a result function ends in a return");
    let IrExpr::Return(Some(value)) = ir.exprs[last as usize] else {
        unreachable!("a result function ends in a return");
    };
    let mut statements = Vec::new();
    if let Some(information) = &facts.source_information {
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        let key = ir.add_expr(IrExpr::Const(IrConst::Int(facts.key)));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(information.as_str().into())));
        // kotlinc builds the marker's arguments without offsets, so its line begins at the call.
        for argument in [composer, key, text] {
            ir.mark_positionless(argument);
        }
        let start = runtime::call(
            ir,
            &runtime.source_information_marker_start,
            None,
            vec![composer, key, text],
        );
        ir.set_source_line(start, facts.opening_line);
        statements.push(start);
    }
    if let Some(trace) = facts.trace.as_deref() {
        let key = ir.add_expr(IrExpr::Const(IrConst::Int(facts.key)));
        let changed = ir.add_expr(IrExpr::GetValue(slots.changed));
        let unused = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
        let text = ir.add_expr(IrExpr::Const(IrConst::String(trace.into())));
        let event = runtime::call(
            ir,
            &runtime.trace_event_start,
            None,
            vec![key, changed, unused, text],
        );
        statements.push(if_trace_in_progress(ir, runtime, event));
    }
    statements.extend(leading.iter().copied());
    let ty = ir.functions[facts.function as usize].ret;
    let returned = if matches!(
        ir.exprs[value as usize],
        IrExpr::Const(_) | IrExpr::GetValue(_)
    ) {
        value
    } else {
        let temporary = next_value_slot(ir, facts.function);
        statements.push(ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty,
            init: Some(value),
            named: false,
        }));
        ir.add_expr(IrExpr::GetValue(temporary))
    };
    if facts.trace.is_some() {
        let event = runtime::call(ir, &runtime.trace_event_end, None, Vec::new());
        statements.push(if_trace_in_progress(ir, runtime, event));
    }
    if facts.source_information.is_some() {
        let composer = ir.add_expr(IrExpr::GetValue(slots.composer));
        ir.mark_positionless(composer);
        let end = runtime::call(
            ir,
            &runtime.source_information_marker_end,
            None,
            vec![composer],
        );
        ir.set_source_line(end, facts.opening_line);
        statements.push(end);
    }
    ir.exprs[last as usize] = IrExpr::Return(Some(returned));
    statements.push(last);
    ir.exprs[body.block as usize] = IrExpr::Block {
        stmts: statements,
        value: None,
    };
}

/// `if (isTraceInProgress()) event`
fn if_trace_in_progress(ir: &mut IrFile, runtime: &ComposeRuntime, event: ExprId) -> ExprId {
    let probe = runtime::call(ir, &runtime.is_trace_in_progress, None, Vec::new());
    if_then(ir, probe, event)
}

/// `$composer.endRestartGroup()?.updateScope { $composer, $force -> F($composer, …) }`, as common
/// lowering realizes a safe call: the receiver in a temporary, then a null-guarded selector.
fn update_scope(ir: &mut IrFile, runtime: &ComposeRuntime, facts: &RestartableFunction) -> ExprId {
    let composer = ir.add_expr(IrExpr::GetValue(Slots::of(facts).composer));
    let ended = runtime::call(ir, &runtime.end_restart_group, Some(composer), Vec::new());
    let scope_ty = runtime.end_restart_group.ret;
    // Past `$dirty` too, which the body block does not hold yet.
    let slots = Slots::of(facts);
    let temporary = next_value_slot(ir, facts.function).max(slots.composer + slots.generated());
    let variable = ir.add_expr(IrExpr::Variable {
        index: temporary,
        ty: scope_ty,
        init: Some(ended),
        named: false,
    });
    let read = ir.add_expr(IrExpr::GetValue(temporary));
    let receiver = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: read,
        type_operand: scope_ty.non_null(),
    });
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let tested = ir.add_expr(IrExpr::GetValue(temporary));
    let condition = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Eq,
        lhs: tested,
        rhs: null,
    });
    let lambda = recompose_lambda(ir, runtime, facts);
    let update = runtime::call(ir, &runtime.update_scope, Some(receiver), vec![lambda]);
    let null_result = ir.add_expr(IrExpr::UnitInstance);
    let guarded = ir.add_expr(IrExpr::When {
        branches: vec![(Some(condition), null_result), (None, update)],
    });
    ir.whens.exhaustive.insert(guarded, Ty::Unit);
    ir.set_logical_type(guarded, Ty::Unit);
    ir.null_guards.insert(guarded);
    let block = ir.add_expr(IrExpr::Block {
        stmts: vec![variable],
        value: Some(guarded),
    });
    ir.safe_calls.insert(block);
    block
}

/// `{ $composer, $force -> F(p…, $composer, updateChangedFlags($changed or 1)) }`, capturing the
/// parameters and `$changed`, and numbered as the only lambda lifted out of `F` (kotlinc's
/// `F$lambda$0`).
fn recompose_lambda(
    ir: &mut IrFile,
    runtime: &ComposeRuntime,
    facts: &RestartableFunction,
) -> ExprId {
    let function = facts.function;
    let slots = Slots::of(facts);
    // The implementation's values: the captured parameters, `$changed` and `$default`, then
    // `$composer` and `$force`.
    let parameters = facts.parameters.len() as u32;
    let defaults = slots.default.is_some();
    let ints = slots.changed_ints;
    let captured = parameters + ints + u32::from(defaults);
    let mut args = (0..parameters)
        .map(|value| ir.add_expr(IrExpr::GetValue(value)))
        .collect::<Vec<_>>();
    let composer = ir.add_expr(IrExpr::GetValue(captured));
    args.push(composer);
    // The first int forces the recomposition; the others pass on as they are.
    for int in 0..ints {
        let changed = ir.add_expr(IrExpr::GetValue(parameters + int));
        let state = if int == 0 {
            let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
            or(ir, changed, one)
        } else {
            changed
        };
        args.push(runtime::call(
            ir,
            &runtime.update_changed_flags,
            None,
            vec![state],
        ));
    }
    if defaults {
        args.push(ir.add_expr(IrExpr::GetValue(parameters + ints)));
    }
    let restart = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Local(function)),
        dispatch_receiver: None,
        args,
    });
    let unit = ir.add_expr(IrExpr::UnitInstance);
    let result = ir.add_expr(IrExpr::Return(Some(unit)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![restart, result],
        value: None,
    });
    // The target names the implementation from its lifting site; this one is only provisional.
    let name = format!("{}$recompose", ir.functions[function as usize].name);
    let mut params = facts
        .parameters
        .iter()
        .map(physical_type)
        .collect::<Vec<_>>();
    params.extend((0..ints).map(|_| Ty::Int));
    params.extend(slots.default.map(|_| Ty::Int));
    params.extend([runtime::composer(), Ty::Int]);
    let param_checks = vec![None; params.len()];
    let implementation = ir.add_fun(IrFunction {
        name,
        params,
        ret: Ty::obj("kotlin/Unit"),
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks,
    });
    ir.set_method_visibility(implementation, crate::types::Visibility::Private);
    // kotlinc writes the recompose lambda's locals but no line of its own.
    let capture = crate::ir::IrValueCapture {
        declaration: crate::ir::IrCapturedDeclaration::Parameter,
        capturer: crate::ir::IrCapturingCallable::Lambda,
        crossinline_parameter: None,
    };
    let mut parameter_identities = (0u32..)
        .zip(
            facts
                .parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .chain((0..ints).map(|int| int_name("$changed", int)))
                .chain(defaults.then(|| "$default".to_string())),
        )
        .map(|(ordinal, name)| IrParameterIdentity::captured_value(Some(name), ordinal, capture))
        .collect::<Vec<_>>();
    parameter_identities.extend([
        IrParameterIdentity::producer_value("$composer"),
        IrParameterIdentity::producer_value("$force"),
    ]);
    ir.publish_generated_facade_function(crate::ir::IrGeneratedFunctionPublication {
        function: implementation,
        parameter_identities,
        metadata: None,
        debug: crate::ir::IrGeneratedDeclarationDebug::LocalsOnly,
    });
    let captures = (0..parameters)
        .chain((0..ints).map(|int| slots.changed_int(int)))
        .chain(slots.default)
        .map(|value| ir.add_expr(IrExpr::GetValue(value)))
        .collect();
    let lambda = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 2,
        captures,
        sam: None,
        inline_body: None,
    });
    ir.note_synthesized_lambda(lambda);
    let (sequence, source_order) = facts.lifting.clone();
    ir.place_generated_lambda(
        sequence,
        source_order,
        IrEnclosure::Function(function),
        implementation,
    );
    lambda
}

/// `@ComposableInferredTarget(scheme)`, and `@FunctionKeyMeta(key, startOffset, endOffset)`, which
/// tooling maps a group key back to its declaration with.
fn annotate_key(ir: &mut IrFile, facts: &RestartableFunction) {
    let annotations = ir.function_annotations.entry(facts.function).or_default();
    // The inferred scheme is visible to the module's dependents, which check their calls with it.
    if let Some(scheme) = &facts.inferred_target {
        annotations.push_metadata_visible(runtime::composable_inferred_target(scheme));
    }
    if facts.key_meta {
        annotations.push(runtime::function_key_meta(facts.key, facts.offsets));
    }
}

/// The first value slot past every one the function's body uses.
fn next_value_slot(ir: &IrFile, function: FunId) -> u32 {
    let declaration = &ir.functions[function as usize];
    let mut next = declaration.params.len() as u32;
    let mut pending = declaration.body.into_iter().collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        match &ir.exprs[expression as usize] {
            IrExpr::Variable { index, .. } => next = next.max(index + 1),
            IrExpr::Try { catches, .. } => {
                next = catches
                    .iter()
                    .fold(next, |next, catch| next.max(catch.var + 1));
            }
            IrExpr::Lambda { captures, .. } => {
                pending.extend(captures.iter().copied());
                continue;
            }
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    next
}

fn mark_line(ir: &mut IrFile, expression: ExprId, line: u32) {
    if line != 0 {
        ir.set_statement_line(expression, line);
    }
}

/// The arity of a composable lambda that takes `values` of its own, then the composer and its
/// `$changed` mask. A lambda's arity is a byte in the IR, as the JVM's parameter limit bounds it.
fn composer_arity(values: usize) -> u8 {
    u8::try_from(values + 2).expect("a composable lambda's arity fits the JVM parameter limit")
}
