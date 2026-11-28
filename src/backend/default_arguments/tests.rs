use super::*;
use crate::fir::{CallableId, ResolvedFunctionOverrideTarget};
use crate::ir::{
    Callee, ExprId, FnParamInfo, FunId, IrBinOp, IrConst, IrExpr, IrFunction, IrSuperCallKind,
};
use crate::types::{type_name, Ty, TypeName};

fn declare(
    ir: &mut IrFile,
    name: &str,
    params: Vec<Ty>,
    ret: Ty,
    body: Option<ExprId>,
    dispatch_receiver: Option<TypeName>,
) -> FunId {
    ir.add_fun(IrFunction {
        name: name.to_owned(),
        params,
        ret,
        body,
        is_static: dispatch_receiver.is_none(),
        dispatch_receiver,
        param_checks: Vec::new(),
    })
}

fn int(ir: &mut IrFile, value: i32) -> ExprId {
    ir.add_expr(IrExpr::Const(IrConst::Int(value)))
}

fn with_defaults(ir: &mut IrFile, function: FunId, defaults: Vec<Option<ExprId>>) {
    let names = (0..defaults.len())
        .map(|ordinal| format!("p{ordinal}"))
        .collect();
    ir.fn_params
        .insert(function, FnParamInfo::source_defaults(names, defaults));
}

/// A member `function` of the class `owner`, as its class lists it.
fn method_of(ir: &mut IrFile, owner: &str, function: FunId) -> u32 {
    let mut class = crate::ir::test_support::blank_class(owner);
    class.methods.push(function);
    let class = ir.add_class(class);
    ir.note_class_method(class, function);
    class
}

fn ty(ty: Ty) -> String {
    match ty {
        Ty::Int => "Int".to_owned(),
        Ty::String => "String".to_owned(),
        other => match other.kind() {
            crate::types::TyKind::Obj(name, _) => name.render(),
            _ => format!("{other:?}"),
        },
    }
}

fn render_all(ir: &IrFile, expressions: &[ExprId]) -> String {
    expressions
        .iter()
        .map(|&expression| render(ir, expression))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The tree under `expression`, in the few node kinds this lowering builds and rewrites.
fn render(ir: &IrFile, expression: ExprId) -> String {
    match ir.expr(expression) {
        IrExpr::Const(IrConst::Int(value)) => value.to_string(),
        IrExpr::Const(IrConst::String(value)) => format!("{:?}", value.to_lossy()),
        IrExpr::GetValue(slot) => format!("${slot}"),
        IrExpr::SetValue { var, value } => format!("${var} = {}", render(ir, *value)),
        IrExpr::Variable {
            index,
            init: Some(init),
            ..
        } => format!("val ${index} = {}", render(ir, *init)),
        IrExpr::AbsentArgument { ty: absent } => format!("absent {}", ty(*absent)),
        IrExpr::Return(Some(value)) => format!("return {}", render(ir, *value)),
        IrExpr::TypeOp {
            op,
            arg,
            type_operand,
        } => format!("({} {op:?} {})", render(ir, *arg), ty(*type_operand)),
        IrExpr::PrimitiveBinOp { op, lhs, rhs } => {
            format!("({} {op:?} {})", render(ir, *lhs), render(ir, *rhs))
        }
        IrExpr::When { branches } => branches
            .iter()
            .map(|(condition, body)| match condition {
                Some(condition) => {
                    format!("if {} then {}", render(ir, *condition), render(ir, *body))
                }
                None => format!("else {}", render(ir, *body)),
            })
            .collect::<Vec<_>>()
            .join(" "),
        IrExpr::Block { stmts, value } => {
            let mut parts = stmts
                .iter()
                .map(|&statement| render(ir, statement))
                .collect::<Vec<_>>();
            parts.extend(value.map(|value| render(ir, value)));
            format!("{{ {} }}", parts.join("; "))
        }
        IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } => {
            let receiver = dispatch_receiver
                .map(|receiver| format!("{}.", render(ir, receiver)))
                .unwrap_or_default();
            let callee = match &**callee {
                Callee::Local(function) => format!("fun{function}"),
                Callee::ClassStatic { owner, function } => {
                    format!("{}.fun{function}", owner.render())
                }
                Callee::ModuleDefaultEntry {
                    target, provider, ..
                } => format!(
                    "entry(target {}, provider {})",
                    target.raw(),
                    provider.raw()
                ),
                other => format!("{other:?}"),
            };
            format!("{receiver}{callee}({})", render_all(ir, args))
        }
        IrExpr::MethodCall {
            class,
            index,
            receiver,
            args,
        } => format!(
            "{}.method{class}:{index}({})",
            render(ir, *receiver),
            args.iter()
                .map(|argument| argument.map_or("_".to_owned(), |argument| render(ir, argument)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => format!("{other:?}"),
    }
}

fn body(ir: &IrFile, function: FunId) -> String {
    render(ir, ir.functions[function as usize].body.expect("a body"))
}

/// The expected fill of parameter slot `slot` from `value` under bit `bit` of mask slot `mask`.
fn fill(mask: u32, bit: i32, slot: u32, value: &str) -> String {
    format!("if ((${mask} BitAnd {bit}) Ne 0) then ${slot} = {value}")
}

#[test]
fn a_suspend_entry_frame_puts_the_masks_before_the_continuation() {
    assert_eq!(
        entry_frame(&["r", "p", "q"], false, ["m"]),
        ["r", "p", "q", "m"]
    );
    assert_eq!(
        entry_frame(&["r", "p", "k"], true, ["m"]),
        ["r", "p", "m", "k"]
    );
}

#[test]
fn masks_take_one_word_per_32_parameters() {
    assert_eq!(mask_words(1), 1);
    assert_eq!(mask_words(32), 1);
    assert_eq!(mask_words(33), 2);
    assert_eq!(omission_masks(3, [0, 2]), vec![0b101]);
    assert_eq!(omission_masks(34, [31, 33]), vec![1 << 31, 0b10]);
    assert_eq!(omission_masks(34, []), vec![0, 0]);
}

/// A dependency declaration lowered into a support unit has no checked identity of its own: the
/// later module reaches it by its export, so an exported declaration with defaults publishes its
/// entry with no caller in the unit, and an unexported one builds none.
#[test]
fn an_exported_dependency_declaration_publishes_its_entry_without_a_caller() {
    let mut ir = IrFile::default();
    let mut declarations = Vec::new();
    for name in ["exported", "unexported"] {
        let read = ir.add_expr(IrExpr::GetValue(0));
        let function = declare(&mut ir, name, vec![Ty::Int], Ty::Int, Some(read), None);
        let seven = int(&mut ir, 7);
        with_defaults(&mut ir, function, vec![Some(seven)]);
        declarations.push(function);
    }
    let [exported_function, unexported_function] = declarations[..] else {
        unreachable!("two declarations");
    };
    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |function| function == exported_function,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&exported_function];
    assert_eq!(ir.default_entries.len(), 1);
    assert_eq!(published_entry(&ir, exported_function), Some(entry));
    assert_eq!(published_entry(&ir, unexported_function), None);
    assert_eq!(ir.functions[entry as usize].name, "exported$default");
    assert_eq!(ir.functions[entry as usize].params, vec![Ty::Int; 2]);
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; return fun{exported_function}($0) }}",
            fill(1, 1, 0, "7")
        )
    );
}

#[test]
fn a_published_entry_takes_the_frame_and_one_mask_word_per_32_parameters() {
    let mut ir = IrFile::default();
    let unit = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let function = declare(
        &mut ir,
        "wide",
        vec![Ty::Int; 34],
        Ty::Unit,
        Some(unit),
        None,
    );
    let first = int(&mut ir, 7);
    let last = int(&mut ir, 8);
    let mut defaults = vec![None; 34];
    defaults[0] = Some(first);
    defaults[33] = Some(last);
    with_defaults(&mut ir, function, defaults);
    ir.checked_callable_functions
        .insert(CallableId::from_raw(4), function);

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    assert_eq!(ir.default_entries.len(), 1);
    let declaration = &ir.functions[entry as usize];
    assert_eq!(declaration.name, "wide$default");
    assert_eq!(ir.synthetic_methods.iter().collect::<Vec<_>>(), [&entry]);
    assert_eq!(declaration.params, vec![Ty::Int; 36]);
    assert_eq!(declaration.ret, Ty::Unit);
    assert_eq!(declaration.dispatch_receiver, None);
    let arguments = (0..34)
        .map(|slot| format!("${slot}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; {}; fun{function}({arguments}) }}",
            fill(34, 1, 0, "7"),
            fill(35, 2, 33, "8"),
        )
    );
    assert_eq!(
        published_identities(&ir, function),
        [CallableId::from_raw(4)]
    );
}

#[test]
fn a_default_reads_an_earlier_parameter_and_its_own_locals_move_past_the_masks() {
    let mut ir = IrFile::default();
    let read = ir.add_expr(IrExpr::GetValue(1));
    let function = declare(
        &mut ir,
        "next",
        vec![Ty::Int, Ty::Int],
        Ty::Int,
        Some(read),
        None,
    );
    let earlier = ir.add_expr(IrExpr::GetValue(0));
    let local = ir.add_expr(IrExpr::Variable {
        index: 2,
        ty: Ty::Int,
        init: Some(earlier),
        named: true,
    });
    let local_read = ir.add_expr(IrExpr::GetValue(2));
    let one = int(&mut ir, 1);
    let sum = ir.add_arithmetic(IrBinOp::Add, local_read, one, Ty::Int);
    let default = ir.add_expr(IrExpr::Block {
        stmts: vec![local],
        value: Some(sum),
    });
    with_defaults(&mut ir, function, vec![None, Some(default)]);
    let supplied = int(&mut ir, 5);
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::LocalWithDefaults {
            function,
            defaults: Box::new([1]),
        }),
        dispatch_receiver: None,
        args: vec![supplied],
    });
    let caller = declare(&mut ir, "caller", Vec::new(), Ty::Int, Some(call), None);

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; return fun{function}($0, $1) }}",
            fill(2, 2, 1, "{ val $3 = $0; ($3 Add 1) }")
        )
    );
    assert_eq!(
        render(&ir, default),
        "{ val $2 = $0; ($2 Add 1) }",
        "the declaration's own default is left as it was"
    );
    assert_eq!(body(&ir, caller), format!("fun{entry}(5, absent Int, 2)"));
}

#[test]
fn an_entry_receives_a_shared_cell_parameter_as_the_declaration_does() {
    let mut ir = IrFile::default();
    let read = ir.add_expr(IrExpr::GetValue(0));
    let function = declare(
        &mut ir,
        "local",
        vec![Ty::Int, Ty::Int],
        Ty::Int,
        Some(read),
        None,
    );
    let default = int(&mut ir, 7);
    with_defaults(&mut ir, function, vec![None, Some(default)]);
    ir.shared_capture_parameters.insert((function, 0), Ty::Int);
    let supplied = int(&mut ir, 5);
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::LocalWithDefaults {
            function,
            defaults: Box::new([1]),
        }),
        dispatch_receiver: None,
        args: vec![supplied],
    });
    declare(&mut ir, "caller", Vec::new(), Ty::Int, Some(call), None);

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    assert_eq!(ir.shared_capture_parameters[&(entry, 0)], Ty::Int);
    assert!(!ir.shared_capture_parameters.contains_key(&(entry, 1)));
}

#[test]
fn a_call_site_passes_every_parameter_and_constant_mask_words() {
    let mut ir = IrFile::default();
    let unit = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let mut params = vec![Ty::Int; 33];
    params.push(Ty::String);
    let function = declare(&mut ir, "wide", params, Ty::Unit, Some(unit), None);
    let mut defaults = vec![None; 34];
    let first = int(&mut ir, 7);
    let text = ir.add_expr(IrExpr::Const(IrConst::String("x".into())));
    defaults[1] = Some(first);
    defaults[33] = Some(text);
    with_defaults(&mut ir, function, defaults);
    let mut args = Vec::new();
    for value in 0..32 {
        args.push(int(&mut ir, value));
    }
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ClassStaticWithDefaults {
            owner: type_name("demo/Holder"),
            function,
            defaults: Box::new([1, 33]),
        }),
        dispatch_receiver: None,
        args,
    });
    let caller = declare(&mut ir, "caller", Vec::new(), Ty::Unit, Some(call), None);

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    let mut expected = vec!["0".to_owned(), "absent Int".to_owned()];
    expected.extend((1..32).map(|value| value.to_string()));
    expected.extend(["absent String".to_owned(), "2".to_owned(), "2".to_owned()]);
    assert_eq!(
        body(&ir, caller),
        format!("fun{entry}({})", expected.join(", "))
    );
    let arguments = (0..34)
        .map(|slot| format!("${slot}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; {}; demo/Holder.fun{function}({arguments}) }}",
            fill(34, 2, 1, "7"),
            fill(35, 2, 33, "\"x\""),
        )
    );
}

#[test]
fn the_entry_of_a_suspend_function_is_a_suspend_function() {
    let mut ir = IrFile::default();
    let parameter = ir.add_expr(IrExpr::GetValue(0));
    let returned = ir.add_expr(IrExpr::Return(Some(parameter)));
    let function = declare(
        &mut ir,
        "pause",
        vec![Ty::Int],
        Ty::Int,
        Some(returned),
        None,
    );
    ir.suspend_funs.push(function);
    let default = int(&mut ir, 3);
    with_defaults(&mut ir, function, vec![Some(default)]);
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::LocalWithDefaults {
            function,
            defaults: Box::new([0]),
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let caller = declare(&mut ir, "caller", Vec::new(), Ty::Int, Some(call), None);
    ir.suspend_funs.push(caller);

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    assert_eq!(body(&ir, caller), format!("fun{entry}(absent Int, 1)"));
    assert_eq!(
        body(&ir, entry),
        format!("{{ {}; return fun{function}($0) }}", fill(1, 1, 0, "3"))
    );
    assert_eq!(ir.suspend_funs, vec![function, caller, entry]);
}

#[test]
fn a_member_entry_takes_the_receiver_and_dispatches_through_the_method() {
    let mut ir = IrFile::default();
    let read = ir.add_expr(IrExpr::GetValue(1));
    let owner = type_name("demo/Shape");
    let function = declare(
        &mut ir,
        "scale",
        vec![Ty::Int],
        Ty::Int,
        Some(read),
        Some(owner),
    );
    let class = method_of(&mut ir, "demo/Shape", function);
    let identity = CallableId::from_raw(42);
    ir.checked_callable_functions.insert(identity, function);
    let two = int(&mut ir, 2);
    with_defaults(&mut ir, function, vec![Some(two)]);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::MethodCall {
        class,
        index: 0,
        receiver,
        args: vec![None],
    });
    let caller = declare(
        &mut ir,
        "caller",
        vec![Ty::obj_name(owner)],
        Ty::Int,
        Some(call),
        None,
    );

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    let declaration = &ir.functions[entry as usize];
    assert_eq!(
        declaration.params,
        vec![Ty::obj_name(owner), Ty::Int, Ty::Int]
    );
    assert_eq!(declaration.dispatch_receiver, None);
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; return $0.method{class}:0($1) }}",
            fill(2, 1, 1, "2")
        )
    );
    assert_eq!(body(&ir, caller), format!("fun{entry}($0, absent Int, 1)"));
    assert_eq!(published_identities(&ir, function), [identity]);
}

#[test]
fn overriding_members_do_not_publish_the_inherited_default_provider_twice() {
    let mut ir = IrFile::default();
    let provider = CallableId::from_raw(20);
    for ordinal in 0..2 {
        let owner = type_name(if ordinal == 0 {
            "demo/First"
        } else {
            "demo/Second"
        });
        let read = ir.add_expr(IrExpr::GetValue(1));
        let function = declare(
            &mut ir,
            "scale",
            vec![Ty::Int],
            Ty::Int,
            Some(read),
            Some(owner),
        );
        method_of(
            &mut ir,
            if ordinal == 0 {
                "demo/First"
            } else {
                "demo/Second"
            },
            function,
        );
        let default = int(&mut ir, 2);
        with_defaults(&mut ir, function, vec![Some(default)]);
        ir.fn_params
            .get_mut(&function)
            .unwrap()
            .inherited_default_provider = Some(ResolvedFunctionOverrideTarget::Module(provider));
        let implementation = CallableId::from_raw(21 + ordinal);
        ir.checked_callable_functions
            .insert(implementation, function);
        record_override(&mut ir, owner, implementation, provider);
        assert!(published_identities(&ir, function).is_empty());
    }
}

#[test]
fn an_actual_member_publishes_its_entry_under_its_expect() {
    let expected = CallableId::from_raw(30);
    let actual = CallableId::from_raw(31);
    let mut ir = IrFile::default();
    let owner = type_name("demo/Renderer");
    let read = ir.add_expr(IrExpr::GetValue(1));
    let function = declare(
        &mut ir,
        "render",
        vec![Ty::Int],
        Ty::Int,
        Some(read),
        Some(owner),
    );
    method_of(&mut ir, "demo/Renderer", function);
    ir.checked_callable_functions.insert(actual, function);
    let default = int(&mut ir, 1);
    with_defaults(&mut ir, function, vec![Some(default)]);
    ir.fn_params
        .get_mut(&function)
        .unwrap()
        .inherited_default_provider = Some(ResolvedFunctionOverrideTarget::Module(expected));

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    assert!(ir.default_entries.contains_key(&function));
    assert_eq!(
        published_identities(&ir, function),
        [expected],
        "an actual member overrides nothing; its expect's callers select its entry"
    );
}

/// Record that `implementation`, a member of `owner`, overrides `overridden`.
fn record_override(
    ir: &mut IrFile,
    owner: TypeName,
    implementation: CallableId,
    overridden: CallableId,
) {
    ir.function_overrides
        .entry(owner)
        .or_default()
        .push(crate::ir::IrFunctionOverride {
            implementation: ResolvedFunctionOverrideTarget::Module(implementation),
            implementation_function: None,
            implementation_owner: owner,
            overridden: ResolvedFunctionOverrideTarget::Module(overridden),
            overridden_owner: type_name("demo/Base"),
            overridden_semantic_role: None,
            collection_barrier: None,
            overridden_is_interface: false,
            name: "scale".to_owned(),
            declared_parameters: vec![Ty::Int],
            declared_result: Ty::Int,
            applied_parameters: vec![Ty::Int],
            applied_result: Ty::Int,
            implementation_parameters: vec![Ty::Int],
            implementation_parameter_identities: Vec::new(),
            overridden_parameter_identities: Vec::new(),
            implementation_result: Ty::Int,
            suspend: false,
            has_kotlin_superclass_override: false,
            depth: 1,
        });
}

#[test]
fn a_value_class_member_entry_calls_the_member_directly() {
    let mut ir = IrFile::default();
    let read = ir.add_expr(IrExpr::GetValue(1));
    let owner = type_name("demo/Meters");
    let function = declare(
        &mut ir,
        "plus",
        vec![Ty::Int],
        Ty::Int,
        Some(read),
        Some(owner),
    );
    let class = method_of(&mut ir, "demo/Meters", function);
    ir.classes[class as usize].is_value = true;
    let two = int(&mut ir, 2);
    with_defaults(&mut ir, function, vec![Some(two)]);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::MethodCall {
        class,
        index: 0,
        receiver,
        args: vec![None],
    });
    let caller = declare(
        &mut ir,
        "caller",
        vec![Ty::obj_name(owner)],
        Ty::Int,
        Some(call),
        None,
    );

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    assert_eq!(
        body(&ir, entry),
        format!("{{ {}; return $0.fun{function}($1) }}", fill(2, 1, 1, "2"))
    );
    assert_eq!(body(&ir, caller), format!("fun{entry}($0, absent Int, 1)"));
}

#[test]
fn an_inherited_default_calls_the_entry_of_its_provider_in_this_file() {
    let mut ir = IrFile::default();
    let base_owner = type_name("demo/Base");
    let derived_owner = type_name("demo/Derived");
    let provider = declare(
        &mut ir,
        "describe",
        vec![Ty::String],
        Ty::String,
        None,
        Some(base_owner),
    );
    let base = method_of(&mut ir, "demo/Base", provider);
    ir.classes[base as usize].is_interface = true;
    let text = ir.add_expr(IrExpr::Const(IrConst::String("base".into())));
    with_defaults(&mut ir, provider, vec![Some(text)]);
    let read = ir.add_expr(IrExpr::GetValue(1));
    let target = declare(
        &mut ir,
        "describe",
        vec![Ty::String],
        Ty::String,
        Some(read),
        Some(derived_owner),
    );
    method_of(&mut ir, "demo/Derived", target);
    let provider_id = CallableId::from_raw(10);
    let target_id = CallableId::from_raw(11);
    ir.checked_callable_functions.insert(provider_id, provider);
    ir.checked_callable_functions.insert(target_id, target);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target: target_id,
            default_provider: ResolvedFunctionOverrideTarget::Module(provider_id),
            name: "describe".to_owned(),
            params: vec![Ty::String],
            ret: Ty::String,
            defaults: Box::new([0]),
            dispatch_receiver_ty: Some(Ty::obj_name(base_owner)),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(receiver),
        args: Vec::new(),
    });
    let caller = declare(
        &mut ir,
        "caller",
        vec![Ty::obj_name(derived_owner)],
        Ty::String,
        Some(call),
        None,
    );

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    assert_eq!(ir.default_entries.len(), 1);
    let entry = ir.default_entries[&provider];
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; return $0.method{base}:0($1) }}",
            fill(2, 1, 1, "\"base\"")
        )
    );
    assert_eq!(
        body(&ir, caller),
        format!("fun{entry}($0, absent String, 1)")
    );
}

#[test]
fn an_override_narrowing_its_providers_result_coerces_the_entry_value() {
    let mut ir = IrFile::default();
    let interface_owner = type_name("demo/Maker");
    let class_owner = type_name("demo/StringMaker");
    let formal = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    let provider = declare(
        &mut ir,
        "make",
        vec![Ty::Int],
        formal,
        None,
        Some(interface_owner),
    );
    let interface = method_of(&mut ir, "demo/Maker", provider);
    ir.classes[interface as usize].is_interface = true;
    let size = int(&mut ir, 16);
    with_defaults(&mut ir, provider, vec![Some(size)]);
    let provider_id = CallableId::from_raw(30);
    ir.checked_callable_functions.insert(provider_id, provider);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target: CallableId::from_raw(31),
            default_provider: ResolvedFunctionOverrideTarget::Module(provider_id),
            name: "make".to_owned(),
            params: vec![Ty::Int],
            ret: Ty::String,
            defaults: Box::new([0]),
            dispatch_receiver_ty: Some(Ty::obj_name(interface_owner)),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(receiver),
        args: Vec::new(),
    });
    ir.call_declared_ret.insert(call, Ty::String);
    let caller = declare(
        &mut ir,
        "caller",
        vec![Ty::obj_name(class_owner)],
        Ty::String,
        Some(call),
        None,
    );

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&provider];
    assert_eq!(ir.functions[entry as usize].ret, formal);
    assert_eq!(
        body(&ir, caller),
        format!("(fun{entry}($0, absent Int, 1) ImplicitCoercion String)")
    );
    assert!(!ir.call_declared_ret.contains_key(&call));
}

#[test]
fn an_actual_entry_is_keyed_by_its_expect_and_calls_the_actual() {
    let expect = CallableId::from_raw(2);
    let actual = CallableId::from_raw(1);
    let mut ir = IrFile::default();
    let read = ir.add_expr(IrExpr::GetValue(0));
    let function = declare(
        &mut ir,
        "answer",
        vec![Ty::String],
        Ty::String,
        Some(read),
        None,
    );
    ir.checked_callable_functions.insert(actual, function);
    let text = ir.add_expr(IrExpr::Const(IrConst::String("OK".into())));
    let mut parameters = FnParamInfo::source_defaults(vec!["value".to_owned()], vec![Some(text)]);
    parameters.inherited_default_provider = Some(ResolvedFunctionOverrideTarget::Module(expect));
    ir.fn_params.insert(function, parameters);
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target: actual,
            default_provider: ResolvedFunctionOverrideTarget::Module(expect),
            name: "answer".to_owned(),
            params: vec![Ty::String],
            ret: Ty::String,
            defaults: Box::new([0]),
            dispatch_receiver_ty: None,
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let caller = declare(&mut ir, "caller", Vec::new(), Ty::String, Some(call), None);

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let entry = ir.default_entries[&function];
    assert_eq!(published_identities(&ir, function), [expect]);
    assert_eq!(
        body(&ir, entry),
        format!(
            "{{ {}; return fun{function}($0) }}",
            fill(1, 1, 0, "\"OK\"")
        )
    );
    assert_eq!(
        body(&ir, caller),
        "entry(target 1, provider 2)(absent String, 1)"
    );
    let IrExpr::Call { callee, .. } = ir.expr(call) else {
        panic!("the call stays a call");
    };
    assert_eq!(
        **callee,
        Callee::ModuleDefaultEntry {
            target: actual,
            provider: expect,
            params: vec![Ty::String],
            ret: Ty::String,
            dispatch_receiver_ty: None,
        }
    );
}

fn member_record(
    owner: TypeName,
    parameters: Vec<Ty>,
    result: Ty,
    defaults: Vec<usize>,
) -> crate::ir::IrModuleCallable {
    let parameter_identities = (0..parameters.len())
        .map(|ordinal| crate::ir::IrParameterIdentity::source(format!("p{ordinal}")))
        .collect();
    crate::ir::IrModuleCallable {
        source: crate::ir::IrModuleSource {
            source: crate::fir::SourceFileId::from_raw(0),
            package: type_name("demo"),
        },
        name: "inherited".into(),
        owner: Some(owner),
        owner_kind: Some(crate::ir::IrClassifierKind::Interface),
        visibility: crate::types::Visibility::Public,
        flags: Default::default(),
        parameters: parameters.into_boxed_slice(),
        default_parameters: defaults.into_boxed_slice(),
        type_parameters: Box::default(),
        owner_type_parameters: Box::default(),
        parameter_identities,
        result,
        annotations: Box::default(),
        placement: crate::ir::IrStaticPlacement::Package,
        overridden_results: Box::default(),
        overridden_declarations: Box::default(),
    }
}

#[test]
fn inherited_member_defaults_in_another_file_keep_receiver_out_of_masks() {
    for (parameter_count, expected_masks) in [
        (1, vec![1]),
        (32, vec![i32::MIN]),
        (33, vec![0, 1]),
        (64, vec![0, i32::MIN]),
    ] {
        let mut ir = IrFile::default();
        let owner = type_name("demo/Host");
        let provider = CallableId::from_raw(20);
        let target = CallableId::from_raw(21);
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        ir.set_logical_type(receiver, Ty::obj_name(owner));
        let supplied = (0..parameter_count - 1)
            .map(|index| ir.add_expr(IrExpr::Const(IrConst::Int(index as i32))))
            .collect::<Vec<_>>();
        let parameters = vec![Ty::Int; parameter_count];
        ir.referenced_module_callables.insert(
            provider,
            member_record(
                owner,
                parameters.clone(),
                Ty::Unit,
                vec![parameter_count - 1],
            ),
        );
        let call = ir.add_expr(IrExpr::Call {
            callee: Box::new(Callee::ModuleWithDefaults {
                target,
                default_provider: ResolvedFunctionOverrideTarget::Module(provider),
                name: "inherited".to_owned(),
                params: parameters.clone(),
                ret: Ty::Unit,
                defaults: vec![parameter_count as u32 - 1].into_boxed_slice(),
                dispatch_receiver_ty: Some(Ty::obj_name(owner)),
                extension_receiver_parameter: None,
            }),
            dispatch_receiver: Some(receiver),
            args: supplied.clone(),
        });
        declare(
            &mut ir,
            "caller",
            vec![Ty::obj_name(owner)],
            Ty::Unit,
            Some(call),
            None,
        );
        let functions = ir.functions.len();

        realize(
            &mut ir,
            &mut CheckedBackendCallables::default(),
            |_| false,
            |_| false,
            &[],
        );

        let IrExpr::Call {
            callee,
            dispatch_receiver,
            args,
        } = ir.expr(call)
        else {
            panic!("the inherited default remains an ordinary call");
        };
        assert_eq!(
            **callee,
            Callee::ModuleDefaultEntry {
                target,
                provider,
                params: parameters,
                ret: Ty::Unit,
                dispatch_receiver_ty: Some(Ty::obj_name(owner)),
            }
        );
        assert_eq!(*dispatch_receiver, None);
        assert_eq!(args[0], receiver);
        assert_eq!(&args[1..parameter_count], supplied.as_slice());
        let IrExpr::AbsentArgument { ty } = ir.expr(args[parameter_count]) else {
            panic!("the omitted semantic parameter retains its typed absent operand");
        };
        assert_eq!(*ty, Ty::Int);
        assert_eq!(args.len(), 1 + parameter_count + expected_masks.len());
        let actual_masks = args[1 + parameter_count..]
            .iter()
            .map(|argument| {
                let IrExpr::Const(IrConst::Int(mask)) = ir.expr(*argument) else {
                    panic!("a complete omission-mask word");
                };
                *mask
            })
            .collect::<Vec<_>>();
        assert_eq!(actual_masks, expected_masks);
        assert_eq!(
            args.iter()
                .filter(|&&argument| argument == receiver)
                .count(),
            1
        );
        assert_eq!(ir.functions.len(), functions);
        assert!(ir.default_entries.is_empty());
    }
}

#[test]
fn cross_file_member_defaults_use_the_provider_frame_before_coercing_the_result() {
    let mut ir = IrFile::default();
    let owner = type_name("demo/Host");
    let provider = CallableId::from_raw(20);
    let target = CallableId::from_raw(21);
    let any = Ty::nullable(Ty::obj("kotlin/Any"));
    let element = Ty::ty_param("provider-element", any);
    let mut record = member_record(owner, vec![element, element], element, vec![1]);
    record.owner_type_parameters = vec![crate::ir::IrCallableTypeParameter {
        semantic_name: "provider-element".into(),
        bounds: vec![(any, false)].into_boxed_slice(),
    }]
    .into_boxed_slice();
    ir.referenced_module_callables.insert(provider, record);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(receiver, Ty::obj_name(owner));
    let supplied = int(&mut ir, 7);
    ir.set_logical_type(supplied, Ty::Int);
    let site = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target,
            default_provider: ResolvedFunctionOverrideTarget::Module(provider),
            name: "inherited".into(),
            params: vec![Ty::Int, Ty::Int],
            ret: Ty::Int,
            defaults: Box::new([1]),
            dispatch_receiver_ty: Some(Ty::obj_name(owner)),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(receiver),
        args: vec![supplied],
    });
    ir.set_logical_type(site, Ty::Int);
    ir.call_declared_ret.insert(site, Ty::Int);
    declare(
        &mut ir,
        "caller",
        vec![Ty::obj_name(owner)],
        Ty::Int,
        Some(site),
        None,
    );

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    let IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: call,
        type_operand,
    } = ir.expr(site)
    else {
        panic!("the caller coerces the provider's result to its checked result");
    };
    assert_eq!(*type_operand, Ty::Int);
    assert_eq!(ir.checked_type(*call), Some(element));
    let IrExpr::Call {
        callee,
        dispatch_receiver,
        args,
    } = ir.expr(*call)
    else {
        panic!("the exact default provider is an ordinary call");
    };
    assert_eq!(
        **callee,
        Callee::ModuleDefaultEntry {
            target,
            provider,
            params: vec![element, element],
            ret: element,
            dispatch_receiver_ty: Some(Ty::obj_name(owner)),
        }
    );
    assert_eq!(*dispatch_receiver, None);
    assert_eq!(args.len(), 4);
    assert_eq!(&args[..2], &[receiver, supplied]);
    let IrExpr::AbsentArgument { ty } = ir.expr(args[2]) else {
        panic!("the provider owns the omitted operand's type");
    };
    assert_eq!(*ty, element);
    let IrExpr::Const(IrConst::Int(mask)) = ir.expr(args[3]) else {
        panic!("a complete omission mask");
    };
    assert_eq!(*mask, 2);
}

#[test]
fn shapes_this_lowering_cannot_realize_keep_their_checked_form() {
    let mut ir = IrFile::default();
    let owner = type_name("demo/Host");
    let provider = CallableId::from_raw(20);
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let extension = ir.add_expr(IrExpr::GetValue(1));
    // A member extension: its receivers do not fit the entry's frame.
    let member_extension = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target: provider,
            default_provider: ResolvedFunctionOverrideTarget::Module(provider),
            name: "extended".to_owned(),
            params: vec![Ty::String, Ty::Int],
            ret: Ty::Unit,
            defaults: Box::new([1]),
            dispatch_receiver_ty: Some(Ty::obj_name(owner)),
            extension_receiver_parameter: Some(0),
        }),
        dispatch_receiver: Some(receiver),
        args: vec![extension],
    });
    // A cross-file default call without its frozen provider declaration.
    let foreign_provider = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target: CallableId::from_raw(21),
            default_provider: ResolvedFunctionOverrideTarget::Module(provider),
            name: "inherited".to_owned(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            defaults: Box::new([0]),
            dispatch_receiver_ty: Some(Ty::obj_name(owner)),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(receiver),
        args: Vec::new(),
    });
    // A `super` call with an omitted argument.
    let super_call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::Super {
            qualifier: owner,
            owner,
            dispatch_owner: owner,
            enclosing_dispatch: false,
            kind: IrSuperCallKind::Function,
            name: "base".to_owned(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            interface: false,
            realization: crate::libraries::MemberRealization::Dispatch,
            descriptor: String::new(),
            declaration: Some(ResolvedFunctionOverrideTarget::Module(provider)),
            defaults: vec![0],
            source_member: None,
        }),
        dispatch_receiver: Some(receiver),
        args: Vec::new(),
    });
    // A top-level call whose defaults a dependency provides.
    let dependency_provider = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::ModuleWithDefaults {
            target: CallableId::from_raw(22),
            default_provider: ResolvedFunctionOverrideTarget::External(
                crate::fir::ExternalCallableId::from_raw(3),
            ),
            name: "imported".to_owned(),
            params: vec![Ty::Int],
            ret: Ty::Unit,
            defaults: Box::new([0]),
            dispatch_receiver_ty: None,
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    let block = ir.add_expr(IrExpr::Block {
        stmts: vec![
            member_extension,
            foreign_provider,
            super_call,
            dependency_provider,
        ],
        value: None,
    });
    declare(
        &mut ir,
        "caller",
        vec![Ty::obj_name(owner), Ty::String],
        Ty::Unit,
        Some(block),
        None,
    );
    let before = format!("{:?}", ir.exprs);
    let functions = ir.functions.len();

    realize(
        &mut ir,
        &mut CheckedBackendCallables::default(),
        |_| false,
        |_| false,
        &[],
    );

    assert_eq!(format!("{:?}", ir.exprs), before);
    assert_eq!(ir.functions.len(), functions);
    assert!(ir.default_entries.is_empty());
}

/// Answers one dependency member of a singleton, as a provider publishes it.
struct SingletonMember(crate::fir::ExternalCallableId);

impl crate::symbol_source::SymbolSource for SingletonMember {
    fn external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
    ) -> Option<crate::libraries::ExternalCallableRealization> {
        (identity == self.0).then(|| crate::libraries::ExternalCallableRealization {
            callable: crate::libraries::LibraryCallable::library(
                type_name("dep/Registry"),
                "next",
                Vec::new(),
                Ty::Int,
                Ty::Int,
                "()I",
            ),
            kind: crate::libraries::ExternalCallableKind::Member,
            declaration_owner: None,
            parameter_identities: Box::new([]),
            declaration_signature: None,
            dependency_callable: None,
            direct_c_function: None,
            klib_body_companion_receiver: None,
            klib_body_type_parameters: Box::new([]),
            klib_body_declaration_modifiers: None,
            klib_body_class_contract: None,
            klib_body_accessor: None,
            klib_body_vararg_index: None,
            klib_body_parameter_defaults: None,
            declares_parameter_defaults: false,
            composable_parameters: Default::default(),
        })
    }
}

#[test]
fn an_entry_keeps_the_checked_facts_frozen_for_its_defaults() {
    let mut ir = IrFile::default();
    let target = crate::fir::ExternalCallableId::from_raw(9);
    let registry = type_name("dep/Registry");
    let unit = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let function = declare(&mut ir, "take", vec![Ty::Int], Ty::Unit, Some(unit), None);
    // `fun take(id: Int = Registry.next())`, whose receiver the dependency erases.
    let next = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target,
            default_provider: None,
            params: Vec::new(),
            ret: Ty::Int,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: None,
        args: Vec::new(),
    });
    ir.external_singleton_dispatches.insert(next, registry);
    with_defaults(&mut ir, function, vec![Some(next)]);
    ir.checked_callable_functions
        .insert(CallableId::from_raw(4), function);
    let mut facts = CheckedBackendCallables::freeze(&ir, &SingletonMember(target))
        .expect("the selected dependency member has a frozen fact");

    realize(&mut ir, &mut facts, |_| false, |_| false, &[]);

    let entry = ir.default_entries[&function];
    assert_eq!(ir.default_entries.len(), 1);
    assert_eq!(ir.functions[entry as usize].params, vec![Ty::Int, Ty::Int]);
    let IrExpr::Block { stmts, value: None } = ir.expr(ir.functions[entry as usize].body.unwrap())
    else {
        panic!("an entry body is a block of statements");
    };
    assert_eq!(stmts.len(), 2);
    assert_eq!(render(&ir, stmts[1]), format!("fun{function}($0)"));
    let IrExpr::When { branches } = ir.expr(stmts[0]) else {
        panic!("a fill is guarded by its mask bit");
    };
    assert_eq!(branches.len(), 1);
    let (condition, fill) = branches[0];
    assert_eq!(
        render(&ir, condition.expect("a guarded fill")),
        "(($1 BitAnd 1) Ne 0)"
    );
    let IrExpr::SetValue {
        var: 0,
        value: call,
    } = *ir.expr(fill)
    else {
        panic!("a fill sets the omitted parameter's slot");
    };
    // The entry evaluates a copy of the checked call, which carries the original's frozen
    // singleton dispatch; the declaration's own default keeps it as well.
    assert_ne!(call, next);
    assert_eq!(render(&ir, call), render(&ir, next));
    let dispatch = Some(crate::libraries::SingletonDispatch {
        classifier: registry,
    });
    assert_eq!(facts.singleton_dispatch(call), dispatch);
    assert_eq!(facts.singleton_dispatch(next), dispatch);
}

mod constructors;
