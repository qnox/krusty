//! A call to a same-module inline function, inlined from the body this emission compiles for the
//! callee: kotlinc's `IrSourceCompilerForInline`, which generates the callee's `MethodNode` and
//! hands it to the same `MethodInliner` a classpath callee goes through.
//!
//! Common lowering decided each such call's route before emission and recorded the calls that
//! take this one ([`IrFile::compiled_inline_call`]). Each callee is compiled once per pass, into a
//! writer of its own class and source file, before any caller is emitted; the inliner then reads
//! that body exactly as it reads a dependency's. The route is final: a body that cannot be
//! compiled or inlined fails the emission rather than becoming a real call.

use std::rc::Rc;

use super::*;

/// One same-module inline function, compiled to the method kotlinc inlines.
pub(in crate::jvm::ir_emit) struct CompiledInlineCallee {
    owner: String,
    name: String,
    descriptor: String,
    body: crate::jvm::classreader::MethodCode,
}

/// The callees of one emission pass, compiled before any of their callers.
#[derive(Default)]
pub(in crate::jvm::ir_emit) struct CompiledInlineCallees(
    std::cell::RefCell<HashMap<u32, Result<Rc<CompiledInlineCallee>, &'static str>>>,
);

/// Compile every function this file inlines from its compiled body, replacing the previous
/// pass's bodies.
pub(in crate::jvm::ir_emit) fn compile_inline_callees(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) {
    let mut functions = Vec::new();
    for (call, function) in ir.compiled_inline_calls() {
        if !names_compiled_callee(ir, call, function) {
            env.run.set_emit_error(format!(
                "expression {call} is recorded as inlining function {function}'s compiled body \
                 but is not a call to it"
            ));
            return;
        }
        functions.push(function);
    }
    functions.sort_unstable();
    functions.dedup();
    env.run.compiled_inline_callees.0.borrow_mut().clear();
    // kotlinc compiles a callee's node when a call first needs it, so a callee whose body inlines
    // another same-module callee reads that one's node, compiled first.
    for function in callees_before_callers(ir, &functions) {
        let callee = compile_callee(ir, function, facade, env, opts).map(Rc::new);
        env.run
            .compiled_inline_callees
            .0
            .borrow_mut()
            .insert(function, callee);
    }
}

/// `functions` ordered so each comes after every same-module callee its own body inlines from a
/// compiled body, in the order the bodies are first reached.
fn callees_before_callers(ir: &IrFile, functions: &[u32]) -> Vec<u32> {
    let mut ordered = Vec::with_capacity(functions.len());
    let mut visited = std::collections::HashSet::new();
    for &function in functions {
        visit_callee(ir, function, &mut visited, &mut ordered);
    }
    ordered
}

fn visit_callee(
    ir: &IrFile,
    function: u32,
    visited: &mut std::collections::HashSet<u32>,
    ordered: &mut Vec<u32>,
) {
    if !visited.insert(function) {
        return;
    }
    let mut nested = Vec::new();
    let mut pending = ir
        .functions
        .get(function as usize)
        .and_then(|function| function.body)
        .into_iter()
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(callee) = ir.compiled_inline_call(expression) {
            nested.push(callee);
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    nested.sort_unstable();
    for callee in nested {
        visit_callee(ir, callee, visited, ordered);
    }
    ordered.push(function);
}

/// Whether `call` is a call of `function`: a same-file call names it, a call realized onto
/// another file's facade names its checked module callable.
fn names_compiled_callee(ir: &IrFile, call: u32, function: u32) -> bool {
    match ir.expr(call) {
        IrExpr::Call {
            callee: Callee::Local(callee),
            dispatch_receiver: None,
            ..
        } => *callee == function,
        IrExpr::Call {
            callee:
                Callee::CrossFile {
                    module_target: Some(target),
                    module_default_call: false,
                    ..
                },
            dispatch_receiver: None,
            ..
        } => ir.checked_callable_functions.get(target) == Some(&function),
        _ => false,
    }
}

/// Compile `function` alone into a writer for the class that declares it, named for its source
/// file, and read back the method it emitted.
fn compile_callee(
    ir: &IrFile,
    function: u32,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
) -> Result<CompiledInlineCallee, &'static str> {
    // A same-file callee belongs to this file's facade; another file's template names the facade
    // and source unit common lowering recorded for it.
    let (source, owner) = if ir.foreign_inline_templates.contains(&function) {
        let source = ir
            .foreign_template_source(function)
            .ok_or("a compiled inline callee of another file has no recorded source file")?
            .source;
        let declaring = ir
            .foreign_template_facade(function)
            .ok_or("a compiled inline callee of another file has no resolved facade")?;
        let owner = crate::jvm::names::classfile_internal_name_of(declaring).to_owned();
        (source, owner)
    } else {
        let source = ir
            .source_debug
            .current()
            .ok_or("a compiled inline callee's file has no source unit")?;
        (source, facade.to_owned())
    };
    let file = ir
        .source_debug
        .file(source)
        .ok_or("a compiled inline callee's source file is not in the module")?;
    let mut cw = new_writer(&owner, "java/lang/Object", opts);
    cw.set_source_file(Some(file.name.to_string()));
    // A callee of another file numbers what it inlines past that file's lines, not the caller's.
    cw.start_source_map(u16::try_from(file.line_count).unwrap_or(u16::MAX).max(1));
    emit_method_inner(
        ir,
        function,
        StaticOwner::Facade,
        &owner,
        facade,
        &mut cw,
        false,
        env,
    );
    function_debug::attach_declared_function_debug(
        ir,
        env.override_results,
        function,
        &owner,
        &mut cw,
    );
    let method = cw
        .sole_emitted_method_for_inlining()
        .ok_or("a compiled inline callee's body does not read back as one method")?;
    Ok(CompiledInlineCallee {
        owner,
        name: method.name,
        descriptor: method.descriptor,
        body: method.body,
    })
}

impl Emitter<'_> {
    /// Inline the call `call_expression`, which lowering routed to the compiled body of its
    /// same-module callee, with the arguments `args`.
    pub(in crate::jvm::ir_emit) fn inline_compiled_module_call(
        &mut self,
        call_expression: u32,
        args: &[u32],
        code: &mut CodeBuilder,
    ) {
        let function = self
            .ir
            .compiled_inline_call(call_expression)
            .expect("a call routed to a compiled inline body names its callee");
        let compiled = self
            .run
            .compiled_inline_callees
            .0
            .borrow()
            .get(&function)
            .cloned()
            .unwrap_or(Err(
                "a compiled inline callee was not compiled before its callers",
            ));
        let callee = match compiled {
            Ok(callee) => callee,
            Err(reason) => {
                self.run.set_inline_bail(reason);
                return;
            }
        };
        let target = super::super::inline_call::InlineStaticTarget {
            owner: &callee.owner,
            name: &callee.name,
            descriptor: &callee.descriptor,
            splice_desc: &callee.descriptor,
            inline_only: false,
            allow_owner_bridge: true,
            for_inline_copy: false,
        };
        let reified = crate::jvm::reified_operations::splice_arguments(
            self.ir,
            call_expression,
            &self.facade,
            self.ir.recursive_type_of,
            &|ty| self.rendered_inlined_cast_target(ty),
        );
        let call = ClasspathInlineCall {
            call_expression,
            target: &target,
            args,
            leading_non_argument_operands: 0,
            body: &callee.body,
            reified: &reified,
            defaults: None,
        };
        let call_frame = self.frame.mark();
        let base = self.frame.size().max(code.max_locals);
        let inlined = self.inline_call_with_body(&call, base, code);
        // The call's temporaries go with its frame, as kotlinc's `leaveTemps` does.
        self.frame.drop_to(call_frame);
        match inlined {
            InlineCallOutcome::Handled | InlineCallOutcome::HandledWithError => {}
            InlineCallOutcome::NotApplicable => self
                .run
                .set_inline_bail("the inliner does not cover a compiled same-module inline body"),
        }
    }
}
