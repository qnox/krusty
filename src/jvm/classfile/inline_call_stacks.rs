//! kotlinc's mandatory stack normalization for a method that is not a coroutine
//! (`OptimizationMethodVisitor.performTransformations`: `FixStackWithLabelNormalization`, then
//! `UninitializedStoresProcessor`), run when its class is written.
//!
//! The emitter brackets an inlined body that must be entered with an empty operand stack
//! (`requiresEmptyStackOnEntry`) with `InlineMarker.beforeInlineCall`/`afterInlineCall` when the
//! caller has already pushed operands (see `ir_emit::bytecode_inline_call`). FixStack stores those
//! operands into locals above every other at the opening marker and reloads them under the call's
//! result at the closing one; an object a `new` left among them is then constructed after its
//! arguments instead. A coroutine runs the same passes inside its own transformation, so only the
//! other methods are handled here. The normalized body then goes through kotlinc's optimizer like
//! a transformed coroutine's.
//!
//! The pass works on the whole finished method, so the saved operands take slots above every slot
//! the method uses, as kotlinc's do, wherever the markers came from.
//!
//! Normalization is not optional once the markers are written: a handler entered over the caller's
//! operands loses them, so the body as emitted need not even verify. A method the passes cannot
//! normalize is therefore reported as a failure of the class, never written as emitted.

use super::ClassWriter;
use crate::jvm::bytecode::{instruction_len, CodegenMarker, CODEGEN_MARKER_OP};
use crate::jvm::bytecode_passes::coroutines::process_uninitialized_stores;
use crate::jvm::bytecode_passes::fix_stack::fix_stack;
use crate::jvm::bytecode_passes::insn_list::EditableMethod;

use super::constant_pool_queries::PoolLookup;

/// The offsets of the `beforeInlineCall`/`afterInlineCall` markers in `code`, or `None` when the
/// body does not decode.
fn inline_call_markers(code: &[u8]) -> Option<Vec<usize>> {
    let mut found = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let len = instruction_len(code, pc)?;
        if code[pc] == CODEGEN_MARKER_OP
            && matches!(
                CodegenMarker::from_operand(*code.get(pc + 1)?),
                Some(CodegenMarker::BeforeInlineCall | CodegenMarker::AfterInlineCall)
            )
        {
            found.push(pc);
        }
        pc += len;
    }
    Some(found)
}

impl ClassWriter {
    /// Normalize the operand stack around the inline calls of every method that saved it and is
    /// not a transformed coroutine. Each method that cannot be normalized is reported, by name and
    /// descriptor, with the reason.
    pub(super) fn fix_inline_call_stacks(&mut self) -> Vec<String> {
        let mut failures = Vec::new();
        for index in 0..self.methods.len() {
            let method = &self.methods[index];
            if method
                .rewrite_source
                .as_deref()
                .is_some_and(|source| source.transformed.is_some())
            {
                continue;
            }
            let Some(code) = method.code.as_deref() else {
                continue;
            };
            let outcome = match inline_call_markers(code) {
                Some(markers) if markers.is_empty() => continue,
                Some(_) => self.fix_inline_call_stack(index),
                None => Err("the body does not decode".to_string()),
            };
            if let Err(reason) = outcome {
                let method = &self.methods[index];
                let name = self.cp.utf8_at(method.name).unwrap_or("?");
                let desc = self.cp.utf8_at(method.desc).unwrap_or("?");
                failures.push(format!(
                    "{}.{name}{desc}: the operand stack around its inline calls cannot be saved: \
                     {reason}",
                    self.internal_name
                ));
            }
        }
        failures
    }

    fn fix_inline_call_stack(&mut self, index: usize) -> Result<(), String> {
        let method = &self.methods[index];
        let bytes = method.code.as_deref().ok_or("the method has no body")?;
        let source = method
            .rewrite_source
            .as_deref()
            .ok_or("the method has no rewrite source")?;
        let (access, name, desc) = (source.access, source.name.clone(), source.desc.clone());
        let pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let node = self
            .finished_node(method, source, bytes, &pool)
            .ok_or("the finished body cannot be read")?
            .node;
        let mut body = EditableMethod::new(node);
        fix_stack(&mut body, &self.internal_name)
            .map_err(|error| format!("FixStack failed: {error:?}"))?;
        process_uninitialized_stores(&mut body)
            .map_err(|error| format!("the uninitialized stores cannot be moved: {error:?}"))?;
        self.install_transformed(index, access, &name, &desc, body.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::inline_call_markers;
    use crate::jvm::bytecode::CODEGEN_MARKER_OP;
    use crate::jvm::classfile::{ClassWriter, CodeBuilder, ACC_PUBLIC, ACC_STATIC};

    /// `static int f()` whose bracketed "inline call" leaves two values instead of one result:
    /// FixStack cannot restore the saved operand under it.
    fn writer_with_unnormalizable_markers() -> ClassWriter {
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        code.push_int(1, &mut writer);
        code.inline_call_marker(true);
        code.push_int(2, &mut writer);
        code.push_int(3, &mut writer);
        code.inline_call_marker(false);
        code.iadd();
        code.iadd();
        code.ireturn();
        code.link();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "f", "()I", &code);
        writer
    }

    #[test]
    fn a_method_the_passes_cannot_normalize_fails_the_class() {
        let finished = writer_with_unnormalizable_markers().finish_with_coroutines();
        assert!(finished.bytes.is_empty(), "the class is not written");
        assert_eq!(finished.failures.len(), 1, "{:?}", finished.failures);
        assert!(
            finished.failures[0].starts_with("T.f()I: the operand stack around its inline calls"),
            "{:?}",
            finished.failures
        );
    }

    #[test]
    fn only_the_inline_call_markers_are_found() {
        // aload_0; beforeInlineCall; iconst_1; mark(I)V; pop; afterInlineCall; areturn
        let code = [
            0x2a,
            CODEGEN_MARKER_OP,
            2,
            0x04,
            CODEGEN_MARKER_OP,
            1,
            0x57,
            CODEGEN_MARKER_OP,
            3,
            0xb0,
        ];
        assert_eq!(inline_call_markers(&code), Some(vec![1, 7]));
    }
}
