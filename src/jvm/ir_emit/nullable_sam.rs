//! A nullable function value becomes a fun-interface instance only when it is non-null.
//!
//! The adapter's single capture is that function. Wrapping `null` would yield a non-null SAM
//! whose method throws. A lambda literal is a fresh object and does not take this path.

use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

use super::{slot_words, type_descriptor, Emitter, TempRole};

impl<'a> Emitter<'a> {
    /// `new` / captures / `<init>`. A nullable single capture stays null instead of being passed
    /// to the constructor.
    pub(super) fn emit_capturing_lambda_class(
        &mut self,
        code: &mut CodeBuilder,
        internal: &str,
        captures: &[u32],
        cap_tys: &[Ty],
        nullable: bool,
    ) {
        if nullable && captures.len() == 1 {
            self.emit_null_preserving_lambda_class(code, internal, captures[0], cap_tys);
            return;
        }
        self.emit_initialized_lambda_class(code, internal, cap_tys, |emitter, code| {
            for &capture in captures {
                emitter.emit_value(capture, code);
            }
        });
    }

    /// `invokedynamic` over the captures. A nullable single capture skips the call site when null.
    pub(super) fn emit_indy_lambda(
        &mut self,
        code: &mut CodeBuilder,
        indy: u16,
        cap_words: i32,
        captures: &[u32],
        nullable: bool,
    ) {
        if nullable && captures.len() == 1 {
            self.emit_value(captures[0], code);
            let null_case = code.new_label();
            let done = code.new_label();
            code.dup();
            code.ifnull(null_case);
            code.invokedynamic(indy, cap_words, 1);
            code.goto(done);
            code.bind(null_case);
            code.pop();
            code.aconst_null();
            code.bind(done);
            return;
        }
        for &capture in captures {
            self.emit_value(capture, code);
        }
        code.invokedynamic(indy, cap_words, 1);
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
