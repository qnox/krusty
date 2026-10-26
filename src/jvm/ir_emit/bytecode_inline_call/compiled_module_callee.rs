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
    /// Whether the body is an instance method, whose slot 0 holds the call's dispatch receiver.
    instance: bool,
    name: String,
    descriptor: String,
    body: crate::jvm::classreader::MethodCode,
}

/// The callees of one emission pass and the classes they declare, compiled before any of their
/// callers.
#[derive(Default)]
pub(in crate::jvm::ir_emit) struct CompiledInlineCallees {
    functions: std::cell::RefCell<HashMap<u32, Result<Rc<CompiledInlineCallee>, &'static str>>>,
    pub(super) classes: super::compiled_module_classes::CompiledModuleClasses,
}

/// Compile every function this file inlines from its compiled body, replacing the previous
/// pass's bodies.
pub(in crate::jvm::ir_emit) fn compile_inline_callees(
    ir: &IrFile,
    facade: &str,
    env: &EmitEnv,
    opts: &EmitOptions,
    class_meta: &dyn Fn(&str) -> Option<KotlinMetadata>,
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
    env.run
        .compiled_inline_callees
        .functions
        .borrow_mut()
        .clear();
    env.run.compiled_inline_callees.classes.clear();
    // kotlinc compiles a callee's node when a call first needs it, so a callee whose body inlines
    // another same-module callee reads that one's node, compiled first.
    for function in callees_before_callers(ir, &functions) {
        // Objects the body regenerates for its own inline calls are classes of its file. The
        // callee's callers read them; its file writes them when it emits the function itself,
        // under the same names.
        let names = env.run.regenerated_object_names.snapshot();
        let machines = env.run.machine_classes.borrow().len();
        let callee = compile_callee(ir, function, facade, env, opts).map(Rc::new);
        let regenerated = env.run.machine_classes.borrow_mut().split_off(machines);
        env.run
            .compiled_inline_callees
            .classes
            .read_only(regenerated);
        env.run.regenerated_object_names.restore(names);
        env.run
            .compiled_inline_callees
            .functions
            .borrow_mut()
            .insert(function, callee);
        // The classes its body declares, which its callers' inlining reads and may regenerate.
        super::compiled_module_classes::compile_declared_classes(
            ir, function, facade, env, opts, class_meta,
        );
    }
}

/// `functions` ordered so each comes after every same-module callee its own body, or a class it
/// declares, inlines from a compiled body, in the order the bodies are first reached.
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
    // The callee's own body and the methods of the classes it declares, which are compiled with
    // it and inline their own same-module callees.
    let declared_methods = ir
        .classes
        .iter()
        .filter(|class| {
            class.enclosure.is_some_and(|scope| {
                crate::jvm::inner_classes::scope_within(ir, scope, &|owner| owner == function)
            })
        })
        .flat_map(|class| class.methods.iter().copied());
    let mut pending = std::iter::once(function)
        .chain(declared_methods)
        .filter_map(|function| ir.functions.get(function as usize)?.body)
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

/// Whether `call` is a call of `function`: a same-file call names it (a `companion { … }` block
/// member as a static of its class), a member call names it among its class's methods, and a call
/// realized onto another file's class names its checked module callable.
fn names_compiled_callee(ir: &IrFile, call: u32, function: u32) -> bool {
    match ir.expr(call) {
        IrExpr::Call {
            callee: call_callee,
            dispatch_receiver: None,
            ..
        } if let Callee::Local(callee)
        | Callee::ClassStatic {
            function: callee, ..
        } = &**call_callee =>
        {
            *callee == function
        }
        // A value-class member's call is realized as a static call of its `-impl`, which names
        // the exact function it calls.
        IrExpr::Call {
            callee: call_callee,
            dispatch_receiver: None,
            ..
        } if let Callee::Static { .. } = &**call_callee => {
            ir.jvm_member_targets.get(&call) == Some(&function)
        }
        IrExpr::MethodCall { class, index, .. } => {
            ir.classes
                .get(*class as usize)
                .and_then(|class| class.methods.get(*index as usize))
                == Some(&function)
        }
        IrExpr::Call {
            callee: call_callee,
            dispatch_receiver,
            ..
        } if let (
            Callee::CrossFile {
                module_target: Some(target),
                module_default_call: false,
                ..
            },
            None,
        )
        | (
            Callee::Virtual {
                module_target: Some(target),
                ..
            },
            Some(_),
        ) = (&**call_callee, dispatch_receiver) =>
        {
            ir.checked_callable_functions.get(target) == Some(&function)
        }
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
    // A member is compiled on its class as that class emits it: an instance method, or a static
    // one (a value class's `-impl`) whose receiver is its first parameter. Another file's member
    // template names the class that declares it. Any other same-file callee belongs to this
    // file's facade, and another file's template names the facade and source unit common
    // lowering recorded for it.
    let declaring_class = ir
        .classes
        .iter()
        .find(|class| class.methods.contains(&function))
        .map(|class| class.fq_name)
        .or_else(|| {
            ir.foreign_inline_templates
                .contains(&function)
                .then(|| ir.functions[function as usize].dispatch_receiver)
                .flatten()
        });
    let foreign = ir.foreign_inline_templates.contains(&function);
    let source = if foreign {
        ir.foreign_template_source(function)
            .ok_or("a compiled inline callee of another file has no recorded source file")?
            .source
    } else {
        ir.source_debug
            .current()
            .ok_or("a compiled inline callee's file has no source unit")?
    };
    let owner = match declaring_class {
        Some(class) => crate::jvm::names::classfile_internal_name_of(class).to_owned(),
        None if foreign => crate::jvm::names::classfile_internal_name_of(
            ir.foreign_template_facade(function)
                .ok_or("a compiled inline callee of another file has no resolved facade")?,
        )
        .to_owned(),
        None => facade.to_owned(),
    };
    let instance = declaring_class.is_some() && !ir.functions[function as usize].is_static;
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
        StaticOwner::of(declaring_class),
        &owner,
        facade,
        &mut cw,
        instance,
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
        instance,
        name: method.name,
        descriptor: method.descriptor,
        body: method.body,
    })
}

impl Emitter<'_> {
    /// Inline the call `call_expression`, which lowering routed to the compiled body of its
    /// same-module callee. A member's dispatch receiver is the body's first operand, its `this`.
    pub(in crate::jvm::ir_emit) fn inline_compiled_module_call(
        &mut self,
        call_expression: u32,
        code: &mut CodeBuilder,
    ) {
        let function = self
            .ir
            .compiled_inline_call(call_expression)
            .expect("a call routed to a compiled inline body names its callee");
        let operands = self
            .ir
            .compiled_inline_operands(call_expression)
            .expect("a call routed to a compiled inline body has operands");
        let compiled = self
            .run
            .compiled_inline_callees
            .functions
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
        if callee.instance != operands.receiver.is_some() {
            self.run.set_inline_bail(
                "a compiled inline callee's dispatch receiver does not match its call",
            );
            return;
        }
        // An instance body reads `this` from slot 0, so the receiver leads its operands.
        let splice_descriptor = if callee.instance {
            format!("(L{};{}", callee.owner, &callee.descriptor[1..])
        } else {
            callee.descriptor.clone()
        };
        let args = operands
            .receiver
            .into_iter()
            .chain(operands.arguments.iter().copied())
            .collect::<Vec<_>>();
        let target = super::super::inline_call::InlineStaticTarget {
            owner: &callee.owner,
            name: &callee.name,
            descriptor: &callee.descriptor,
            splice_desc: &splice_descriptor,
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
            args: &args,
            leading_non_argument_operands: usize::from(callee.instance),
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
