//! Native process-entry emission.
//!
//! The runtime calls one exported `kt_program_entry`. It initializes the collector and the entry
//! file, invokes `main` or `box`, reports uncaught exceptions, prints the requested result, and
//! exits through the runtime ABI.

use super::*;

impl<'a> FileLowering<'a> {
    /// `kt_program_entry`: what the runtime's `_start` calls. Records the stack bottom for the
    /// collector, runs the entry function — printing its result when it has one, which is how a
    /// `box()` case reports its verdict — and exits through the kernel; it never returns to
    /// `_start`.
    ///
    /// A `box()` answer is printed after [`BOX_RESULT_FRAME`], with nothing after it. A harness
    /// requires exactly one frame and reads every byte after it. Its last LINE would not do,
    /// because the program's own output comes first and an answer may span lines: `"FAIL\nOK"` is
    /// a wrong answer whose last line is `OK`.
    pub(super) fn define_program_entry(
        &mut self,
        main_index: usize,
        entry: Entry,
        file_init: FuncId,
        statics_may_throw: bool,
    ) -> Result<(), Unsupported> {
        let void = Signature::new(CallConv::SystemV);
        let entry_id = self
            .module
            .declare_function(PROGRAM_ENTRY, Linkage::Export, &void)
            .map_err(|error| format!("declaring `{PROGRAM_ENTRY}` ({error})"))?;
        let init = self.import("kt_runtime_init", &[Ty::obj("kotlin/Any")], Ty::Unit)?;
        let exit = self.import("kt_exit", &[Ty::Int], Ty::Unit)?;
        let uncaught = self.import("kt_check_uncaught", &[], Ty::Unit)?;
        let prints_result = carrier(self.ir.functions[main_index].ret) == Carrier::Ref;
        let println = if prints_result && entry == Entry::Main {
            Some(self.import("kt_println_any", &[any()], Ty::Unit)?)
        } else {
            None
        };
        let framed = if prints_result && entry == Entry::Box {
            Some((
                self.string_data(BOX_RESULT_FRAME.as_bytes())?,
                self.import("kt_string_utf8", &[Ty::obj("kotlin/Any"), Ty::Int], any())?,
                self.import("kt_print_any", &[any()], Ty::Unit)?,
            ))
        } else {
            None
        };
        let main = self.functions[main_index].expect("the entry function has a body");
        let frontend_config = self.module.target_config();

        let mut context = self.module.make_context();
        context.func.signature = void;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            let block = builder.create_block();
            builder.switch_to_block(block);
            builder.seal_block(block);
            // The address of this frame's own slot is as good a stack bottom as any: everything
            // the program does happens in frames below it.
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let bottom = builder.ins().stack_addr(types::I64, slot, 0);
            let init_ref = self.module.declare_func_in_func(init, builder.func);
            builder.ins().call(init_ref, &[bottom]);
            // This file's top-level properties initialize before the entry, through the same
            // once-only function a cross-file call uses. A second call, from another file, is a
            // return.
            let uncaught_ref = self.module.declare_func_in_func(uncaught, builder.func);
            let file_init_ref = self.module.declare_func_in_func(file_init, builder.func);
            builder.ins().call(file_init_ref, &[]);
            if statics_may_throw {
                // An initializer that threw has left its exception pending. Kotlin never reaches
                // the entry then — the JVM fails the facade's `<clinit>` first — so the program
                // ends here, reporting it, before the entry's first statement can run.
                builder.ins().call(uncaught_ref, &[]);
            }
            let main_ref = self.module.declare_func_in_func(main, builder.func);
            let call = builder.ins().call(main_ref, &[]);
            // A `throw` nothing caught has left the exception pending and returned a zero value
            // all the way to here. Kotlin ends the program reporting it, which is what this does —
            // and it must happen BEFORE the answer is printed, because that zero is not an answer.
            builder.ins().call(uncaught_ref, &[]);
            if let Some(println) = println {
                let result = builder.inst_results(call)[0];
                let println_ref = self.module.declare_func_in_func(println, builder.func);
                builder.ins().call(println_ref, &[result]);
            }
            if let Some((frame, utf8, print)) = framed {
                let result = builder.inst_results(call)[0];
                let global = self.module.declare_data_in_func(frame, builder.func);
                let bytes = builder.ins().symbol_value(types::I64, global);
                let length = builder
                    .ins()
                    .iconst(types::I32, BOX_RESULT_FRAME.len() as i64);
                let utf8_ref = self.module.declare_func_in_func(utf8, builder.func);
                let made = builder.ins().call(utf8_ref, &[bytes, length]);
                let marker = builder.inst_results(made)[0];
                let print_ref = self.module.declare_func_in_func(print, builder.func);
                builder.ins().call(print_ref, &[marker]);
                builder.ins().call(print_ref, &[result]);
            }
            let zero = builder.ins().iconst(types::I32, 0);
            let exit_ref = self.module.declare_func_in_func(exit, builder.func);
            builder.ins().call(exit_ref, &[zero]);
            builder.ins().return_(&[]);
            builder.finalize(frontend_config);
        }
        self.module
            .define_function(entry_id, &mut context)
            .map_err(|error| format!("compiling `{PROGRAM_ENTRY}` ({error})"))?;
        self.module.clear_context(&mut context);
        Ok(())
    }
}
