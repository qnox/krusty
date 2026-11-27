//! `access$getThis$0$p`: the accessor an inner class's outer-instance read takes inside a body a
//! caller may splice into another class.
//!
//! `this$0` is package-private, so a non-private inline function cannot name it: the body is
//! copied into callers in any package. kotlinc reads it through a public synthetic static on the
//! inner class instead, and so does the body itself.

use super::{slot_words, type_descriptor, Emitter};
use crate::value_classes::instance_representation;
use krusty::ir::IrFile;
use krusty::jvm::classfile::{ClassWriter, CodeBuilder};
use krusty::types::TypeName;

const NAME: &str = "access$getThis$0$p";
const FIELD: &str = "this$0";

/// The accessor's descriptor: the inner instance in, the outer instance's representation out.
fn descriptor(ir: &IrFile, inner: TypeName, outer: TypeName) -> String {
    let outer = instance_representation(ir, outer);
    format!("(L{};){}", inner.render(), type_descriptor(outer))
}

/// Declare `inner`'s accessor of its outer instance. Its code carries the inner class's
/// declaration line, as each synthetic accessor kotlinc adds to a class does.
pub(super) fn emit(
    ir: &IrFile,
    inner: TypeName,
    outer: TypeName,
    flags: u16,
    declaration_line: u32,
    cw: &mut ClassWriter,
) {
    let descriptor = descriptor(ir, inner, outer);
    let internal = inner.render();
    let outer = instance_representation(ir, outer);
    let mut code = CodeBuilder::new(1);
    code.mark_line(declaration_line);
    code.aload(0);
    let field = cw.fieldref(&internal, FIELD, &type_descriptor(outer));
    code.getfield(field, slot_words(outer) as i32);
    code.areturn();
    code.ensure_locals(1);
    code.link();
    cw.add_method(flags, NAME, &descriptor, &code);
    cw.set_method_debug(
        NAME,
        &descriptor,
        None,
        &[("$this".to_string(), format!("L{internal};"), 0)],
    );
}

impl Emitter<'_> {
    /// Read the outer instance of `inner`, whose instance is on the stack: through the accessor
    /// when the plan published one for this body, else the field.
    pub(super) fn emit_outer_instance_read(
        &mut self,
        inner: TypeName,
        outer: TypeName,
        code: &mut CodeBuilder,
    ) {
        let representation = instance_representation(self.ir, outer);
        let words = slot_words(representation) as i32;
        let routed = self.export_private_calls
            && self.run.static_accessor_plan.borrow().declares(
                crate::private_static_access::StaticOwner::Class(inner),
                super::static_accessors::StaticAccessor::OuterInstance { inner, outer },
            );
        if routed {
            let method =
                self.cw
                    .methodref(&inner.render(), NAME, &descriptor(self.ir, inner, outer));
            code.invokestatic(method, 1, words);
        } else {
            let field = self
                .cw
                .fieldref(&inner.render(), FIELD, &type_descriptor(representation));
            code.getfield(field, words);
        }
    }
}
