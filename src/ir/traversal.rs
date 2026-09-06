use super::*;

/// Invoke `f` on each direct child expression of `e`. The single structural definition of an
/// `IrExpr`'s sub-expressions — tree walks (index shifting, scans) delegate here so a new variant is
/// covered in one place. Written EXHAUSTIVELY (no `_` arm) on purpose: adding an `IrExpr` variant must
/// fail to compile here rather than silently drop its children from every walk.
pub fn for_each_child(exprs: &[IrExpr], e: ExprId, f: &mut impl FnMut(ExprId)) {
    match &exprs[e as usize] {
        IrExpr::Checked(operation) => match operation {
            IrCheckedOperation::Call {
                dispatch_receiver,
                extension_receiver,
                arguments,
                ..
            } => {
                dispatch_receiver.iter().for_each(|&receiver| f(receiver));
                extension_receiver.iter().for_each(|&receiver| f(receiver));
                arguments.iter().for_each(|argument| match argument {
                    IrCheckedArgument::Expression { value, .. } => f(*value),
                    IrCheckedArgument::Default { .. } => {}
                    IrCheckedArgument::Vararg { elements, .. } => {
                        elements.iter().for_each(|(value, _)| f(*value));
                    }
                });
            }
            IrCheckedOperation::ConstructorDelegation {
                outer_receiver,
                arguments,
                ..
            } => {
                outer_receiver.iter().for_each(|&receiver| f(receiver));
                arguments.iter().for_each(|argument| match argument {
                    IrCheckedArgument::Expression { value, .. } => f(*value),
                    IrCheckedArgument::Default { .. } => {}
                    IrCheckedArgument::Vararg { elements, .. } => {
                        elements.iter().for_each(|(value, _)| f(*value));
                    }
                });
            }
            IrCheckedOperation::BackingFieldRead {
                dispatch_receiver, ..
            }
            | IrCheckedOperation::LateinitFieldRead {
                dispatch_receiver, ..
            } => dispatch_receiver.iter().for_each(|&receiver| f(receiver)),
            IrCheckedOperation::BackingFieldWrite {
                dispatch_receiver,
                value,
                ..
            } => {
                dispatch_receiver.iter().for_each(|&receiver| f(receiver));
                f(*value);
            }
            IrCheckedOperation::PropertyRead {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            } => {
                dispatch_receiver.iter().for_each(|&receiver| f(receiver));
                extension_receiver.iter().for_each(|&receiver| f(receiver));
                context_arguments.iter().for_each(|&receiver| f(receiver));
            }
            IrCheckedOperation::PropertyWrite {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                ..
            } => {
                dispatch_receiver.iter().for_each(|&receiver| f(receiver));
                extension_receiver.iter().for_each(|&receiver| f(receiver));
                context_arguments.iter().for_each(|&receiver| f(receiver));
                f(*value);
            }
            IrCheckedOperation::ExternalPropertyRead {
                receiver,
                arguments,
                ..
            }
            | IrCheckedOperation::ExternalPropertyWrite {
                receiver,
                arguments,
                ..
            } => {
                receiver.iter().for_each(|&receiver| f(receiver));
                arguments.iter().for_each(|&argument| f(argument));
            }
            IrCheckedOperation::RangeConstruction { start, end, .. } => {
                f(*start);
                f(*end);
            }
            IrCheckedOperation::RangeContains {
                value, start, end, ..
            } => {
                f(*value);
                f(*start);
                f(*end);
            }
            IrCheckedOperation::RangeLoop {
                start, end, body, ..
            } => {
                f(*start);
                f(*end);
                f(*body);
            }
            IrCheckedOperation::CallableReference {
                dispatch_receiver,
                extension_receiver,
                ..
            }
            | IrCheckedOperation::PropertyReference {
                dispatch_receiver,
                extension_receiver,
                ..
            } => {
                dispatch_receiver.iter().for_each(|&receiver| f(receiver));
                extension_receiver.iter().for_each(|&receiver| f(receiver));
            }
        },
        IrExpr::CallableReference(reference) => {
            reference.captures.iter().for_each(|&capture| f(capture));
            reference
                .bound_receiver
                .iter()
                .for_each(|&receiver| f(receiver));
        }
        IrExpr::Block { stmts, value } => {
            stmts.iter().for_each(|&s| f(s));
            value.iter().for_each(|&v| f(v));
        }
        IrExpr::When { branches } => branches.iter().for_each(|(c, b)| {
            c.iter().for_each(|&c| f(c));
            f(*b);
        }),
        IrExpr::Return(v) => v.iter().for_each(|&v| f(v)),
        IrExpr::TypeOp { arg, .. }
        | IrExpr::NotNullAssert { operand: arg, .. }
        | IrExpr::LateinitCheck { operand: arg, .. }
        | IrExpr::Throw { operand: arg }
        | IrExpr::EnumValueOf { arg, .. }
        | IrExpr::ReifiedTypeOp { arg, .. }
        | IrExpr::RefNew { init: arg, .. }
        | IrExpr::RefGet { holder: arg, .. }
        | IrExpr::NewArray { size: arg, .. }
        | IrExpr::PrimitiveNeg { operand: arg, .. } => f(*arg),
        IrExpr::StringConcat(parts) => parts.iter().for_each(|&p| f(p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            f(*lhs);
            f(*rhs);
        }
        IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => f(*value),
        IrExpr::SetField {
            receiver, value, ..
        }
        | IrExpr::RefSet {
            holder: receiver,
            value,
            ..
        } => {
            f(*receiver);
            f(*value);
        }
        IrExpr::PropertyWrite {
            receiver, value, ..
        } => {
            receiver.iter().for_each(|&receiver| f(receiver));
            f(*value);
        }
        IrExpr::Variable { init, .. } => init.iter().for_each(|&i| f(i)),
        IrExpr::EnclosingInstance { receiver, .. }
        | IrExpr::GetField { receiver, .. }
        | IrExpr::LateinitInitialized { receiver, .. } => f(*receiver),
        IrExpr::PropertyRead { receiver, .. } => receiver.iter().for_each(|&receiver| f(receiver)),
        IrExpr::Call {
            args,
            dispatch_receiver,
            ..
        } => {
            dispatch_receiver.iter().for_each(|&r| f(r));
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            f(*receiver);
            args.iter().flatten().for_each(|&a| f(a));
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            f(*func);
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::New { args, .. } | IrExpr::Vararg { elements: args, .. } => {
            args.iter().for_each(|&a| f(a))
        }
        IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } => {
            captures.iter().for_each(|&c| f(c));
            inline_body.iter().for_each(|&b| f(b));
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            f(*cond);
            f(*body);
            update.iter().for_each(|&u| f(u));
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            f(*body);
            catches.iter().for_each(|c| f(c.body));
            finally.iter().for_each(|&fin| f(fin));
        }
        IrExpr::PluginPlaceholder { exprs: kids, .. } => kids.iter().for_each(|&k| f(k)),
        IrExpr::KClassLiteral { value, .. } => value.iter().for_each(|&value| f(value)),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::LocalPropertyReference { .. }
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::GetStatic(_)
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::EnumEntries { .. }
        | IrExpr::ReifiedClassMarker { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation => {}
    }
}

/// Whether evaluating `expr` can run NO code at all — a literal or a local read. Strictly conservative:
/// anything that touches a type (a static read, an enum entry, a singleton) can trigger that type's
/// initializer, which is arbitrary user code, so it is NOT in this set. Used where a value the source
/// program computes is discarded by the target form and the consumer must decide whether it still has to
/// be evaluated.
pub fn expr_runs_no_code(ir: &IrFile, expr: ExprId) -> bool {
    matches!(
        ir.expr(expr),
        IrExpr::Const(_) | IrExpr::ClassConst { .. } | IrExpr::GetValue(_) | IrExpr::UnitInstance
    )
}

/// Collect the expressions that supply the value of `expression` after transparent control-flow
/// containers are removed. Representation-sensitive consumers must classify these tails rather than
/// independently teaching themselves how `Block` and `When` carry values.
pub fn value_tails(exprs: &[IrExpr], expression: ExprId, out: &mut Vec<ExprId>) {
    match &exprs[expression as usize] {
        IrExpr::When { branches } => {
            for &(_, result) in branches {
                value_tails(exprs, result, out);
            }
        }
        IrExpr::Block {
            value: Some(value), ..
        } => value_tails(exprs, *value, out),
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                value_tails(exprs, last, out);
            }
        }
        _ => out.push(expression),
    }
}

/// Whether a top-level `foo$default` synthetic can be safely emitted for `fid`. Every registered
/// default expression must be self-contained in the synthetic frame: no unresolved invocation and no
/// reference to a value index beyond the parameters or locals declared by the default itself. Physical
/// value-class adaptation is a backend responsibility and is therefore not guessed here.
pub fn toplevel_default_stub_safe(ir: &IrFile, fid: u32) -> bool {
    let f = &ir.functions[fid as usize];
    // A user function literally named `<name>$default` (a back-ticked identifier) would collide with the
    // synthetic — don't emit the stub (kotlinc also treats that as a conflicting declaration).
    let stub_name = format!("{}$default", f.name);
    if ir
        .functions
        .iter()
        .any(|g| g.dispatch_receiver.is_none() && g.name == stub_name)
    {
        return false;
    }
    // Overloaded top-level functions may all have `<name>$default` siblings; the descriptor selects the
    // concrete overload, just as it does for the real method. The lowerer reaches this path only after the
    // checker has selected a source declaration / function id.
    let n = f.params.len() as u32;
    let Some(defaults) = ir.param_defaults(fid) else {
        return false;
    };
    defaults.iter().enumerate().all(|(parameter, default)| {
        let Some(default) = default else {
            return true;
        };
        let expression_safe = default_expr_stub_safe(ir, *default, n);
        crate::trace_compiler!(
            "lower",
            "default stub safety function={} parameter={} expression={} expression_safe={} slot={:?} logical={:?}",
            f.name,
            parameter,
            default,
            expression_safe,
            f.params.get(parameter),
            ir.logical_types.get(default),
        );
        expression_safe
    })
}

fn default_expr_stub_safe(ir: &IrFile, e: ExprId, n: u32) -> bool {
    let mut locals = std::collections::HashSet::new();
    collect_default_locals(ir, e, &mut locals);
    default_expr_stub_safe_with_locals(ir, e, n, &locals)
}

/// Collect value slots declared by the default expression itself. These slots are valid in the
/// synthetic stub frame: the emitter visits their `Variable`/`catch` declarations before their
/// uses. A lambda's inline body has its own value namespace and is not emitted by the stub, so only
/// the lambda's capture expressions belong to this walk.
fn collect_default_locals(ir: &IrFile, e: ExprId, locals: &mut std::collections::HashSet<u32>) {
    match &ir.exprs[e as usize] {
        IrExpr::Variable { index, .. } => {
            locals.insert(*index);
        }
        IrExpr::Try { catches, .. } => {
            locals.extend(catches.iter().map(|catch| catch.var));
        }
        _ => {}
    }
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        for &capture in captures {
            collect_default_locals(ir, capture, locals);
        }
    } else {
        for_each_child(&ir.exprs, e, &mut |child| {
            collect_default_locals(ir, child, locals);
        });
    }
}

fn default_expr_stub_safe_with_locals(
    ir: &IrFile,
    e: ExprId,
    n: u32,
    locals: &std::collections::HashSet<u32>,
) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i >= n && !locals.contains(i) => {
            return false;
        }
        IrExpr::SetValue { var, .. } if *var >= n && !locals.contains(var) => {
            return false;
        }
        // A plain `new`/object construction (`f: F = F()`) is fine — the stub re-emits it. A
        // VALUE/inline-class construction is also re-emittable: the target representation pass
        // rewrites and adapts it to the physical default-stub slot.
        // A default LAMBDA (`f: (Int) -> Int = { it + 1 }`) is re-emittable: the closure construction
        // only reads its captures, and those are checked as children (a capture of a spilled temp /
        // enclosing local — any value `>= n` — rejects above). Its `inline_body` is in the lambda's OWN
        // value numbering, but it is never emitted by the stub (the stub instantiates the closure), so
        // a false child rejection there is merely conservative. A callable-ref (`RefNew`) or an
        // `invoke` still reaches state the static stub layout doesn't carry.
        IrExpr::Lambda { .. } => {}
        IrExpr::RefNew { .. } | IrExpr::InvokeFunction { .. } => {
            return false;
        }
        IrExpr::Call {
            callee: Callee::Static { name, .. },
            ..
        } if name.contains('-')
            // Only a construction that THIS pass rewrote is known safe. Generated-looking spellings
            // are not reserved: accepting them by string would let an unrelated user/library call
            // bypass the conservative mangled-call gate.
            && !ir.erased_value_constructions.contains_key(&e) =>
        {
            // EXCEPT a mangled call whose checked RESULT is a value class
            // (`timeout: Duration = 60.seconds` — a classpath companion-extension getter): the
            // mangling means the physical return already IS the erased underlying, which is
            // a representation the target pass can adapt to the selected stub slot. Both gate runs
            // see the same semantic result evidence — a classpath callee carries its mangled JVM
            // name before and after the value-class pass.
            let vc_result = ir
                .logical_types
                .get(&e)
                .and_then(|t| t.non_null().obj_internal())
                .filter(|&n| ir.is_value_class_name(n));
            let exact_physical_result =
                ir.logical_types.contains_key(&e) && ir.physical_types.contains_key(&e);
            if !exact_physical_result && vc_result.is_none() {
                return false;
            }
        }
        _ => {}
    }
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        captures
            .iter()
            .all(|&capture| default_expr_stub_safe_with_locals(ir, capture, n, locals))
    } else {
        let mut ok = true;
        for_each_child(&ir.exprs, e, &mut |child| {
            if !default_expr_stub_safe_with_locals(ir, child, n, locals) {
                ok = false;
            }
        });
        ok
    }
}

/// Shift every value index (`GetValue`/`SetValue`/`Variable`) `>= threshold` by `by`, throughout the
/// expression tree rooted at `e`. Used when a pass **appends parameters** to a function: the body's
/// locals (numbered from the old parameter count) must move up by the number of new parameters so
/// they don't collide with the inserted parameter slots.
pub fn shift_value_indices(ir: &mut IrFile, e: ExprId, threshold: u32, by: u32) {
    fn shift(
        ir: &mut IrFile,
        e: ExprId,
        threshold: u32,
        by: u32,
        visited: &mut std::collections::HashSet<ExprId>,
    ) {
        // Operands may be shared deliberately (for example, the receiver of the read and write halves
        // of an access increment). The arena is therefore a DAG, not necessarily a tree: rewrite each
        // expression once even when multiple parents reference it.
        if !visited.insert(e) {
            return;
        }
        match &mut ir.exprs[e as usize] {
            IrExpr::GetValue(i) if *i >= threshold => *i += by,
            IrExpr::SetValue { var, .. } if *var >= threshold => *var += by,
            IrExpr::Variable { index, .. } if *index >= threshold => *index += by,
            // A `catch (e) { … }` variable is DECLARED by the `IrCatch.var` field (not a `Variable`
            // node); its uses inside the catch body are shifted by the recursion below, so the field
            // must shift too or the binding and its reads desync.
            IrExpr::Try { catches, .. } => {
                for c in catches.iter_mut() {
                    if c.var >= threshold {
                        c.var += by;
                    }
                }
            }
            _ => {}
        }
        // A nested `Lambda`'s CAPTURES reference the ENCLOSING scope's value slots (shift them), but
        // its `inline_body` is a copy of the lambda's own body in the lambda's OWN value numbering
        // (captures + params) — recursing into it would corrupt those internal slots with this
        // enclosing threshold/delta. So for a `Lambda`, shift only the captures (the impl method's
        // body is a separate function, already untouched here).
        if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
            let caps = captures.clone();
            for c in caps {
                shift(ir, c, threshold, by, visited);
            }
            return;
        }
        let mut kids = Vec::new();
        for_each_child(&ir.exprs, e, &mut |c| kids.push(c));
        for c in kids {
            shift(ir, c, threshold, by, visited);
        }
    }

    let mut visited = std::collections::HashSet::new();
    shift(ir, e, threshold, by, &mut visited);
}
