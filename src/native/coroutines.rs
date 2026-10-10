//! Suspend functions on the native target: the continuation-passing signature, the frame class
//! each suspending body gets, and the protocol pieces the shared state machine asks for.
//!
//! Common IR keeps a `suspend fun` plain. This pass gives every one the CPS signature Kotlin/Native
//! uses: a trailing `Continuation` parameter and an `Any?` result that is either the value or
//! `COROUTINE_SUSPENDED`. A body without suspension points needs nothing more, because the native
//! lowering already coerces a returned value to the declared `Any?`. A body with them is moved into
//! the `invokeSuspend` of a frame class built here, by the target-neutral machine in
//! `backend::coroutines::state_machine`; the function itself only allocates that frame, stores its
//! parameters, and runs the machine once.
//!
//! The frame keeps every parameter and local in a typed field, so nothing is proven live across a
//! suspension. That costs memory and a field access per use, and buys a lowering linear in the body,
//! which is the trade this target makes.
//!
//! A frame class is the program's own class, so the runtime reaches its `resumeWith` and `context`
//! through the thunks its descriptor publishes (see `KType.continuation_resume_with`); the lowering
//! learns which methods those are from [`NativeCoroutines`].

use std::collections::{HashMap, HashSet};

use crate::backend::coroutines::{
    build_state_machine, desugar_tail_suspend, desugar_value_try, desugar_value_when,
    expr_calls_suspend, function_value_types, hoist_operand_suspensions, hoist_suspensions,
    linearize_finally_returns, linearize_suspending_finally, normalize_block_inits,
    normalize_statement_try_results, promote_diverging_tail_to_statement,
    separate_catches_from_finally, splice_return_blocks, test_suspending_conditions_in_body,
    CoroutineAbi, CoroutineRepresentation, FinallySuspension, MachineFrame, MachineInput,
    SuspensionTyping,
};
use crate::ir::{
    for_each_child, Callee, ClassId, ExprId, FunId, IrCatch, IrClass, IrConst,
    IrCoroutineOperation, IrExpr, IrField, IrFile, IrFunction, IrIntrinsic, IrTypeOp,
};
use crate::types::{Ty, TypeName};

type Unsupported = String;

/// The members of a frame class the runtime calls through its descriptor.
#[derive(Clone, Copy, Debug)]
pub(super) struct FrameMembers {
    pub(super) resume_with: FunId,
    pub(super) context: FunId,
}

/// What this pass built that the lowering has to know about.
#[derive(Default)]
pub(super) struct NativeCoroutines {
    pub(super) frames: HashMap<ClassId, FrameMembers>,
}

/// The frame's fixed fields, ahead of one field per parameter and local.
const LABEL: u32 = 0;
const EXCEPTION: u32 = 1;
const COMPLETION: u32 = 2;
const VALUES: u32 = 3;

/// The frame's methods, by index in its `methods`.
const INVOKE_SUSPEND: u32 = 0;

fn any_nullable() -> Ty {
    Ty::nullable(Ty::obj("kotlin/Any"))
}

fn continuation() -> Ty {
    Ty::obj("kotlin/coroutines/Continuation")
}

/// Give every suspend function of `ir` its CPS signature and, where it suspends, its frame class.
/// `unit` names the file, so frame classes of two files in one package never share a name.
pub(super) fn realize(ir: &mut IrFile, unit: &str) -> Result<NativeCoroutines, Unsupported> {
    let mut realized = NativeCoroutines::default();
    if ir.suspend_funs.is_empty() {
        return Ok(realized);
    }
    let suspend_set: HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    // Every function's declared result, before any of them takes the CPS signature: what a
    // suspension point yields is the callee's DECLARED result, not its erased `Any?`.
    let declared: Vec<Ty> = ir.functions.iter().map(|function| function.ret).collect();
    let package = ir.package.as_deref().unwrap_or("").replace('.', "/");
    for fid in ir.suspend_funs.clone() {
        let Some(body) = ir.functions[fid as usize].body else {
            adopt_cps_signature(ir, fid);
            continue;
        };
        if !expr_calls_suspend(ir, body, &suspend_set) {
            let completion = adopt_cps_signature(ir, fid);
            crate::ir::shift_value_indices(ir, body, completion, 1);
            bind_current_continuation(ir, body, completion);
            ensure_tail_return(ir, body, declared[fid as usize]);
            continue;
        }
        normalize(ir, fid, body, &suspend_set, &declared);
        let name = frame_name(&package, unit, &ir.functions[fid as usize].name, fid);
        let (class, members) = build_frame(ir, fid, body, &suspend_set, &declared, name)?;
        realized.frames.insert(class, members);
    }
    Ok(realized)
}

fn frame_name(package: &str, unit: &str, function: &str, fid: FunId) -> TypeName {
    let unit: String = unit
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let simple = format!("{unit}${function}$coroutine${fid}");
    crate::types::type_name(&match package {
        "" => simple,
        package => format!("{package}/{simple}"),
    })
}

/// The trailing `Continuation` parameter and the erased result. Returns the parameter's value index.
fn adopt_cps_signature(ir: &mut IrFile, fid: FunId) -> u32 {
    let function = &mut ir.functions[fid as usize];
    let receiver = u32::from(function.dispatch_receiver.is_some() && !function.is_static);
    let completion = receiver + function.params.len() as u32;
    function.params.push(continuation());
    if !function.param_checks.is_empty() {
        function.param_checks.push(None);
    }
    function.ret = any_nullable();
    completion
}

/// In a body that never suspends, `suspendCoroutineUninterceptedOrReturn`'s continuation is the
/// function's own completion, and `coroutineContext` is its context: there is no frame to resume.
fn bind_current_continuation(ir: &mut IrFile, expression: ExprId, completion: u32) {
    if matches!(ir.exprs[expression as usize], IrExpr::Lambda { .. }) {
        if let IrExpr::Lambda { captures, .. } = ir.exprs[expression as usize].clone() {
            for capture in captures {
                bind_current_continuation(ir, capture, completion);
            }
        }
        return;
    }
    if matches!(ir.exprs[expression as usize], IrExpr::CurrentContinuation) {
        ir.exprs[expression as usize] = IrExpr::GetValue(completion);
        return;
    }
    if matches!(
        ir.exprs[expression as usize],
        IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::CoroutineContext,
                ..
            },
            ..
        }
    ) {
        let continuation = ir.add_expr(IrExpr::GetValue(completion));
        let context = NativeAbi.context(ir, continuation);
        ir.exprs[expression as usize] = ir.exprs[context as usize].clone();
        return;
    }
    let mut children = Vec::new();
    for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
    for child in children {
        bind_current_continuation(ir, child, completion);
    }
}

/// A body that ends in a value returns it, and one that ends in neither a value nor an exit returns
/// `Unit`: the CPS result is a reference either way, which the lowering boxes on the way out.
fn ensure_tail_return(ir: &mut IrFile, body: ExprId, declared: Ty) {
    let IrExpr::Block { mut stmts, value } = ir.exprs[body as usize].clone() else {
        return;
    };
    match value {
        Some(value) if declared != Ty::Unit => {
            stmts.push(ir.add_expr(IrExpr::Return(Some(value))));
        }
        Some(value) => {
            stmts.push(value);
            let unit = ir.add_expr(IrExpr::UnitInstance);
            stmts.push(ir.add_expr(IrExpr::Return(Some(unit))));
        }
        None => {
            let terminates = stmts
                .last()
                .is_some_and(|&last| crate::backend::coroutines::stmt_diverges(ir, last));
            if !terminates {
                let unit = ir.add_expr(IrExpr::UnitInstance);
                stmts.push(ir.add_expr(IrExpr::Return(Some(unit))));
            }
        }
    }
    unit_returns(ir, &mut stmts);
    ir.exprs[body as usize] = IrExpr::Block { stmts, value: None };
}

/// A bare `return` in a `Unit` suspend function returns the `Unit` object.
fn unit_returns(ir: &mut IrFile, statements: &mut [ExprId]) {
    for &statement in statements.iter() {
        rewrite_unit_returns(ir, statement);
    }
}

fn rewrite_unit_returns(ir: &mut IrFile, expression: ExprId) {
    match ir.exprs[expression as usize] {
        IrExpr::Lambda { .. } => {}
        IrExpr::Return(None) => {
            let unit = ir.add_expr(IrExpr::UnitInstance);
            ir.exprs[expression as usize] = IrExpr::Return(Some(unit));
        }
        _ => {
            let mut children = Vec::new();
            for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
            for child in children {
                rewrite_unit_returns(ir, child);
            }
        }
    }
}

/// The target-neutral normalizations that leave every suspension point at a position the machine
/// splits: a statement, a local's initializer, or an assignment's value.
fn normalize(
    ir: &mut IrFile,
    fid: FunId,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    declared: &[Ty],
) {
    let typing = SuspensionTyping {
        orig_rets: declared,
        representation: &NativeAbi,
    };
    let result = declared[fid as usize];
    crate::ir::make_expression_children_unique_tracked(ir, body);
    splice_return_blocks(ir, body);
    separate_catches_from_finally(ir, body);
    let mut value_types = function_value_types(ir, fid, body);
    hoist_suspensions(ir, body, suspend_set, &typing, &mut value_types);
    let suspending_tail = matches!(&ir.exprs[body as usize],
        IrExpr::Block { value: Some(value), .. } if expr_calls_suspend(ir, *value, suspend_set));
    if suspending_tail {
        ensure_tail_return(ir, body, result);
        splice_return_blocks(ir, body);
    }
    desugar_value_try(ir, &NativeAbi, body, suspend_set, &result);
    desugar_value_when(ir, &NativeAbi, body, suspend_set, &result);
    normalize_statement_try_results(ir, body, true);
    linearize_finally_returns(ir, &NativeAbi, body, &result);
    linearize_suspending_finally(
        ir,
        &NativeAbi,
        body,
        suspend_set,
        FinallySuspension::Anywhere,
    );
    test_suspending_conditions_in_body(ir, body, suspend_set);
    normalize_block_inits(ir, body);
    let mut value_types = function_value_types(ir, fid, body);
    hoist_suspensions(ir, body, suspend_set, &typing, &mut value_types);
    promote_diverging_tail_to_statement(ir, body);
    desugar_tail_suspend(ir, body, suspend_set, &result);
    ensure_tail_return(ir, body, result);
    let mut value_types = function_value_types(ir, fid, body);
    hoist_operand_suspensions(ir, body, suspend_set, &typing, &mut value_types);
}

/// Every value index `body` binds a `catch` to, with its type.
fn catch_values(ir: &IrFile, expression: ExprId, out: &mut HashMap<u32, Ty>) {
    match &ir.exprs[expression as usize] {
        IrExpr::Lambda { captures, .. } => {
            for &capture in captures {
                catch_values(ir, capture, out);
            }
            return;
        }
        IrExpr::Try { catches, .. } => {
            for catch in catches {
                out.insert(catch.var, catch.ty);
            }
        }
        _ => {}
    }
    for_each_child(&ir.exprs, expression, &mut |child| {
        catch_values(ir, child, out)
    });
}

/// Build `fid`'s frame class from its normalized `body`, and replace the body with the frame's
/// allocation and first run.
fn build_frame(
    ir: &mut IrFile,
    fid: FunId,
    body: ExprId,
    suspend_set: &HashSet<u32>,
    declared: &[Ty],
    name: TypeName,
) -> Result<(ClassId, FrameMembers), Unsupported> {
    let mut value_types = function_value_types(ir, fid, body);
    catch_values(ir, body, &mut value_types);
    // A captured `var` arrives as its shared holder, not as its value: the frame keeps the holder,
    // so a write through it reaches the enclosing function. The body still reads the holder
    // straight from the parameter here, which is what identifies one.
    let first = u32::from(ir.functions[fid as usize].dispatch_receiver.is_some());
    for (index, carried) in super::captures::carried_parameters(ir, fid)
        .into_iter()
        .enumerate()
    {
        let slot = first + index as u32;
        if let Some(ty) = value_types.get_mut(&slot) {
            *ty = carried;
        }
    }
    let receiver = {
        let function = &ir.functions[fid as usize];
        function.dispatch_receiver.filter(|_| !function.is_static)
    };
    if let Some(owner) = receiver {
        value_types.insert(0, Ty::obj_name(owner));
    }
    let mut indices: Vec<u32> = value_types.keys().copied().collect();
    indices.sort_unstable();

    let mut class = IrClass::generated(name);
    class.fields = vec![
        IrField::new("label".to_string(), Ty::Int),
        IrField::new(
            "exception".to_string(),
            Ty::nullable(Ty::obj("kotlin/Throwable")),
        ),
        IrField::new("completion".to_string(), continuation()),
    ];
    let mut values = HashMap::new();
    // A `Unit` value needs no field: every read of it is the `Unit` object.
    indices.retain(|index| value_types[index] != Ty::Unit);
    for &index in &indices {
        values.insert(index, VALUES + values.len() as u32);
        class
            .fields
            .push(IrField::new(format!("v{index}"), value_types[&index]));
    }
    let class_id = ir.add_class(class);

    let frame = MachineFrame {
        class: class_id,
        label: LABEL,
        exception: EXCEPTION,
        values,
        value_types: value_types.clone(),
    };
    let input = MachineInput {
        body,
        declared_result: declared[fid as usize],
        suspend_set,
        declared_results: declared,
    };
    let machine = build_state_machine(ir, &NativeAbi, &frame, &input)?;
    let invoke_suspend = add_method(
        ir,
        class_id,
        "invokeSuspend",
        vec![any_nullable()],
        any_nullable(),
        machine,
    );
    let resume_with_body = resume_with_body(ir, class_id);
    let resume_with = add_method(
        ir,
        class_id,
        "resumeWith",
        vec![any_nullable()],
        Ty::Unit,
        resume_with_body,
    );
    let context_body = context_body(ir, class_id);
    let context = add_method(
        ir,
        class_id,
        "getContext",
        Vec::new(),
        Ty::obj("kotlin/coroutines/CoroutineContext"),
        context_body,
    );
    debug_assert_eq!(
        ir.classes[class_id as usize].methods[INVOKE_SUSPEND as usize],
        invoke_suspend
    );

    let completion = adopt_cps_signature(ir, fid);
    let entry = entry_body(ir, class_id, completion, &frame.values);
    ir.functions[fid as usize].body = Some(entry);
    Ok((
        class_id,
        FrameMembers {
            resume_with,
            context,
        },
    ))
}

fn add_method(
    ir: &mut IrFile,
    class: ClassId,
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    body: ExprId,
) -> FunId {
    let owner = ir.classes[class as usize].fq_name;
    let checks = vec![None; params.len()];
    let fid = ir.add_fun(IrFunction {
        name: name.to_string(),
        params,
        ret,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: checks,
    });
    ir.classes[class as usize].methods.push(fid);
    ir.note_class_method(class, fid);
    fid
}

fn get_field(ir: &mut IrFile, receiver: u32, class: ClassId, index: u32) -> ExprId {
    let receiver = ir.add_expr(IrExpr::GetValue(receiver));
    ir.add_expr(IrExpr::GetField {
        receiver,
        class,
        index,
    })
}

fn coroutine_operation(
    ir: &mut IrFile,
    operation: IrCoroutineOperation,
    ret: Ty,
    args: Vec<ExprId>,
) -> ExprId {
    ir.add_expr(IrExpr::Call {
        callee: Callee::Intrinsic {
            operation: IrIntrinsic::Coroutine(operation),
            ret,
        },
        dispatch_receiver: None,
        args,
    })
}

/// `resumeWith(result)`: run the machine, and hand what it finishes with to the completion.
///
/// ```text
/// var out: Any? = null
/// try { out = invokeSuspend(result) } catch (e: Throwable) { out = Result.Failure(e) }
/// if (out === COROUTINE_SUSPENDED) return
/// completion.resumeWith(out)
/// ```
fn resume_with_body(ir: &mut IrFile, class: ClassId) -> ExprId {
    const THIS: u32 = 0;
    const RESUMED: u32 = 1;
    const OUT: u32 = 2;
    const CAUGHT: u32 = 3;
    let null = ir.add_expr(IrExpr::Const(IrConst::Null));
    let declare = ir.add_expr(IrExpr::Variable {
        index: OUT,
        ty: any_nullable(),
        init: Some(null),
        named: false,
    });
    let this = ir.add_expr(IrExpr::GetValue(THIS));
    let resumed = ir.add_expr(IrExpr::GetValue(RESUMED));
    let run = ir.add_expr(IrExpr::MethodCall {
        class,
        index: INVOKE_SUSPEND,
        receiver: this,
        args: vec![Some(resumed)],
    });
    let store_run = ir.add_expr(IrExpr::SetValue {
        var: OUT,
        value: run,
    });
    let caught = ir.add_expr(IrExpr::GetValue(CAUGHT));
    let failure = coroutine_operation(
        ir,
        IrCoroutineOperation::Failure,
        any_nullable(),
        vec![caught],
    );
    let store_failure = ir.add_expr(IrExpr::SetValue {
        var: OUT,
        value: failure,
    });
    let guarded = ir.add_expr(IrExpr::Try {
        body: store_run,
        catches: vec![IrCatch {
            var: CAUGHT,
            binding: None,
            ty: Ty::obj("kotlin/Throwable"),
            body: store_failure,
            line: None,
        }],
        finally: None,
        result: Ty::Unit,
    });
    let out = ir.add_expr(IrExpr::GetValue(OUT));
    let marker = coroutine_operation(
        ir,
        IrCoroutineOperation::Suspended,
        any_nullable(),
        Vec::new(),
    );
    let suspended = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::RefEq,
        lhs: out,
        rhs: marker,
    });
    let leave = ir.add_expr(IrExpr::Return(None));
    let check = ir.add_expr(IrExpr::When {
        branches: vec![(Some(suspended), leave)],
    });
    let completion = get_field(ir, THIS, class, COMPLETION);
    let out = ir.add_expr(IrExpr::GetValue(OUT));
    let deliver = coroutine_operation(
        ir,
        IrCoroutineOperation::ResumeWith,
        Ty::Unit,
        vec![completion, out],
    );
    ir.add_expr(IrExpr::Block {
        stmts: vec![declare, guarded, check, deliver],
        value: None,
    })
}

/// `context`: the completion's.
fn context_body(ir: &mut IrFile, class: ClassId) -> ExprId {
    let completion = get_field(ir, 0, class, COMPLETION);
    let context = coroutine_operation(
        ir,
        IrCoroutineOperation::ContextOf,
        Ty::obj("kotlin/coroutines/CoroutineContext"),
        vec![completion],
    );
    let ret = ir.add_expr(IrExpr::Return(Some(context)));
    ir.add_expr(IrExpr::Block {
        stmts: vec![ret],
        value: None,
    })
}

/// The suspend function's own body once its frame holds the rest: allocate the frame, store the
/// completion and every parameter, and run the machine from its first state.
fn entry_body(
    ir: &mut IrFile,
    class: ClassId,
    completion: u32,
    values: &HashMap<u32, u32>,
) -> ExprId {
    let frame = completion + 1;
    let internal = ir.classes[class as usize].fq_name;
    let allocate = ir.add_expr(IrExpr::New {
        internal,
        args: Vec::new(),
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    let mut stmts = vec![ir.add_expr(IrExpr::Variable {
        index: frame,
        ty: Ty::obj_name(internal),
        init: Some(allocate),
        named: false,
    })];
    let mut stores = vec![(completion, COMPLETION)];
    let mut parameters: Vec<(u32, u32)> = values
        .iter()
        .filter(|(&value, _)| value < completion)
        .map(|(&value, &field)| (value, field))
        .collect();
    parameters.sort_unstable();
    stores.extend(parameters);
    for (value, field) in stores {
        let receiver = ir.add_expr(IrExpr::GetValue(frame));
        let value = ir.add_expr(IrExpr::GetValue(value));
        stmts.push(ir.add_expr(IrExpr::SetField {
            receiver,
            class,
            index: field,
            value,
        }));
    }
    let receiver = ir.add_expr(IrExpr::GetValue(frame));
    let unit = ir.add_expr(IrExpr::UnitInstance);
    let run = ir.add_expr(IrExpr::MethodCall {
        class,
        index: INVOKE_SUSPEND,
        receiver,
        args: vec![Some(unit)],
    });
    stmts.push(ir.add_expr(IrExpr::Return(Some(run))));
    ir.add_expr(IrExpr::Block { stmts, value: None })
}

/// The native realization of the coroutine protocol: the marker and the failure encoding are the
/// runtime's (`krusty_coroutines.c`), and a continuation crosses as an ordinary trailing argument.
struct NativeAbi;

impl CoroutineRepresentation for NativeAbi {
    fn zero(&self, ty: &Ty) -> IrConst {
        IrConst::zero_for_value_type(*ty)
    }

    fn throwable(&self) -> crate::types::TypeName {
        crate::types::type_name("kotlin/Throwable")
    }
}

impl CoroutineAbi for NativeAbi {
    fn suspended(&self, ir: &mut IrFile) -> ExprId {
        coroutine_operation(
            ir,
            IrCoroutineOperation::Suspended,
            any_nullable(),
            Vec::new(),
        )
    }

    fn throw_if_failure(&self, ir: &mut IrFile, result: ExprId) -> ExprId {
        coroutine_operation(
            ir,
            IrCoroutineOperation::ThrowOnFailure,
            Ty::Unit,
            vec![result],
        )
    }

    fn resumed_value(&self, ir: &mut IrFile, raw: ExprId, ty: Ty) -> ExprId {
        ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: raw,
            type_operand: ty,
        })
    }

    fn returned_value(&self, _ir: &mut IrFile, value: ExprId, _ty: Ty) -> ExprId {
        // The lowering coerces a returned value to the function's declared `Any?` result.
        value
    }

    fn pass_continuation(
        &self,
        ir: &mut IrFile,
        point: ExprId,
        continuation_value: ExprId,
    ) -> Result<ExprId, Unsupported> {
        if ir.intrinsic_suspension_points.contains_key(&point) {
            return Ok(point);
        }
        match &mut ir.exprs[point as usize] {
            IrExpr::Call { callee, args, .. } => {
                match callee {
                    Callee::Local(_)
                    | Callee::LocalDefault(_)
                    | Callee::LocalWithDefaults { .. }
                    | Callee::ClassStatic { .. }
                    | Callee::ClassStaticDefault { .. }
                    | Callee::ClassStaticWithDefaults { .. } => {}
                    Callee::CrossFile { params, ret, .. }
                    | Callee::Module { params, ret, .. }
                    | Callee::ModuleWithDefaults { params, ret, .. }
                    | Callee::External { params, ret, .. }
                    | Callee::Super { params, ret, .. } => {
                        params.push(continuation());
                        *ret = any_nullable();
                    }
                    Callee::Virtual {
                        params: Some((params, ret)),
                        ..
                    } => {
                        params.push(continuation());
                        *ret = any_nullable();
                    }
                    _ => return Err("a suspend call through this callee".to_string()),
                }
                args.push(continuation_value);
            }
            IrExpr::MethodCall { args, .. } => args.push(Some(continuation_value)),
            IrExpr::InvokeFunction {
                args, params, ret, ..
            } => {
                args.push(continuation_value);
                params.push(continuation());
                *ret = any_nullable();
            }
            _ => return Err("a suspension point that is not a call".to_string()),
        }
        Ok(point)
    }

    fn context(&self, ir: &mut IrFile, continuation_value: ExprId) -> ExprId {
        coroutine_operation(
            ir,
            IrCoroutineOperation::ContextOf,
            Ty::obj("kotlin/coroutines/CoroutineContext"),
            vec![continuation_value],
        )
    }

    fn unknown_state(&self, ir: &mut IrFile) -> ExprId {
        let message = ir.add_expr(IrExpr::Const(IrConst::String(
            "call to 'resume' before 'invoke' with coroutine".into(),
        )));
        let exception = ir.add_expr(IrExpr::New {
            internal: crate::types::type_name("kotlin/IllegalStateException"),
            args: vec![message],
            ctor_params: Some(vec![Ty::nullable(Ty::String)]),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        ir.add_expr(IrExpr::Throw { operand: exception })
    }
}
