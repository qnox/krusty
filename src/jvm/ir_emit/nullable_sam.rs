//! A nullable function value becomes a fun-interface instance only when it is non-null.
//!
//! The adapter's single capture is that function. Wrapping `null` would yield a non-null SAM
//! whose method throws. A lambda literal is a fresh object and does not take this path.

use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

use super::{slot_words, type_descriptor, Emitter, TempRole};

impl<'a> Emitter<'a> {
    /// Emit the JVM-owned nullable-wrapper construction plan for this exact `New`, if any.
    pub(super) fn emit_nullable_sam_wrapper_new(
        &mut self,
        expression: u32,
        internal: crate::types::TypeName,
        args: &[u32],
        code: &mut CodeBuilder,
    ) -> bool {
        if !self
            .sam_wrapper_realizations
            .is_nullable_construction(expression)
        {
            return false;
        }
        let [function] = args else {
            unreachable!("a nullable SAM wrapper is constructed from its function");
        };
        let parameters = self
            .ir
            .class_id_by_name(internal)
            .map(|class| super::class_ctor_jvm_tys(&self.ir.classes[class as usize]))
            .expect("a nullable SAM wrapper names its generated class");
        let &[capture_ty] = parameters.as_slice() else {
            unreachable!("a nullable SAM wrapper constructor takes its function");
        };
        self.emit_nullable_sam_wrapper(code, &internal.render(), *function, capture_ty);
        true
    }

    pub(super) fn nullable_sam_wrapper_emits_control_flow(&self, expression: u32) -> bool {
        self.sam_wrapper_realizations
            .is_nullable_construction(expression)
    }

    /// `new` / captures / `<init>`. A validated nullable adapter's single reference capture stays
    /// null instead of being passed to the constructor.
    pub(super) fn emit_capturing_lambda_class(
        &mut self,
        code: &mut CodeBuilder,
        internal: &str,
        captures: &[u32],
        cap_tys: &[Ty],
        nullable: bool,
    ) {
        if nullable {
            let [function] = captures else {
                unreachable!("validated nullable SAM adapter must have one capture")
            };
            debug_assert_eq!(cap_tys.len(), 1);
            debug_assert_eq!(slot_words(cap_tys[0]), 1);
            self.emit_null_preserving_lambda_class(code, internal, *function, cap_tys);
            return;
        }
        self.emit_initialized_lambda_class(code, internal, cap_tys, |emitter, code| {
            emitter.emit_lambda_captures(captures, cap_tys, code);
        });
    }

    /// `invokedynamic` over the captures. A validated nullable adapter's single reference capture
    /// skips the call site when null.
    pub(super) fn emit_indy_lambda(
        &mut self,
        code: &mut CodeBuilder,
        cap_words: i32,
        captures: &[u32],
        cap_tys: &[Ty],
        nullable: bool,
        site: impl FnOnce(&mut Self) -> u16,
    ) {
        if nullable {
            let [function] = captures else {
                unreachable!("validated nullable SAM adapter must have one capture")
            };
            debug_assert_eq!(cap_words, 1);
            self.emit_value(*function, code);
            let null_case = code.new_label();
            let done = code.new_label();
            code.dup();
            code.ifnull(null_case);
            let indy = site(self);
            code.invokedynamic(indy, cap_words, 1);
            code.goto(done);
            code.bind(null_case);
            code.pop();
            code.aconst_null();
            code.bind(done);
            return;
        }
        self.emit_lambda_captures(captures, cap_tys, code);
        let indy = site(self);
        code.invokedynamic(indy, cap_words, 1);
    }

    /// Bind each capture at the implementation parameter's JVM type. An inlined type parameter is
    /// concrete at this call site and erased on a shared implementation, so a primitive value is
    /// boxed into that parameter.
    fn emit_lambda_captures(&mut self, captures: &[u32], cap_tys: &[Ty], code: &mut CodeBuilder) {
        for (&capture, &parameter) in captures.iter().zip(cap_tys) {
            self.emit_value(capture, code);
            let produced = self.value_ty(capture);
            self.adapt_physical_operand_for(capture, produced, parameter, code);
        }
    }

    /// `new Wrapper(function)` only when `function` is non-null. The wrapper constructor rejects
    /// null, so the conversion itself is the test: evaluate once, store, and leave `null` otherwise.
    pub(super) fn emit_nullable_sam_wrapper(
        &mut self,
        code: &mut CodeBuilder,
        internal: &str,
        function: u32,
        capture_ty: Ty,
    ) {
        debug_assert_eq!(slot_words(capture_ty), 1);
        self.emit_value(function, code);
        let temp = self.frame.enter_temp(TempRole::LambdaCapture, capture_ty);
        let slot = temp.slot();
        super::store(capture_ty, slot, code);
        let null_case = code.new_label();
        let done = code.new_label();
        super::load(capture_ty, slot, code);
        code.ifnull(null_case);
        self.emit_initialized_lambda_class(code, internal, &[capture_ty], |_, code| {
            super::load(capture_ty, slot, code);
        });
        // `<init>` leaves the instance. The null arm is empty until `aconst_null`, so the linear
        // stack height after the constructor does not describe that arm.
        code.goto(done);
        code.bind(null_case);
        code.set_stack(0);
        code.aconst_null();
        code.bind(done);
        self.frame.leave_temp(temp);
    }

    fn emit_null_preserving_lambda_class(
        &mut self,
        code: &mut CodeBuilder,
        internal: &str,
        function: u32,
        cap_tys: &[Ty],
    ) {
        let capture_ty = cap_tys[0];
        self.emit_value(function, code);
        let temp = self.frame.enter_temp(TempRole::LambdaCapture, capture_ty);
        let slot = temp.slot();
        super::store(capture_ty, slot, code);
        let null_case = code.new_label();
        let done = code.new_label();
        super::load(capture_ty, slot, code);
        code.ifnull(null_case);
        self.emit_initialized_lambda_class(code, internal, cap_tys, |_, code| {
            super::load(capture_ty, slot, code);
        });
        // `<init>` leaves the instance. The null arm is empty until `aconst_null`, so the linear
        // stack height after the constructor does not describe that arm.
        code.goto(done);
        code.bind(null_case);
        code.set_stack(0);
        code.aconst_null();
        code.bind(done);
        self.frame.leave_temp(temp);
    }

    fn emit_initialized_lambda_class(
        &mut self,
        code: &mut CodeBuilder,
        internal: &str,
        cap_tys: &[Ty],
        push_captures: impl FnOnce(&mut Self, &mut CodeBuilder),
    ) {
        let class_index = self.cw.class_ref(internal);
        code.new_obj(class_index);
        code.dup();
        push_captures(self, code);
        let cap_descs: String = cap_tys.iter().map(|ty| type_descriptor(*ty)).collect();
        let cap_words: i32 = cap_tys.iter().map(|ty| i32::from(slot_words(*ty))).sum();
        let ctor = self
            .cw
            .methodref(internal, "<init>", &format!("({cap_descs})V"));
        // `arg_words` excludes the receiver, which `invokespecial` accounts for itself.
        code.invokespecial(ctor, cap_words, 0);
    }
}
