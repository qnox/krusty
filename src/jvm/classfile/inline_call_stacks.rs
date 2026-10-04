//! kotlinc's mandatory stack normalization for a method that is not a coroutine
//! (`OptimizationMethodVisitor.performTransformations`: `FixStackWithLabelNormalization`, then
//! `UninitializedStoresProcessor`), run when its class is written.
//!
//! The emitter brackets an inlined body that must be entered with an empty operand stack with
//! `InlineMarker.beforeInlineCall`/`afterInlineCall` when the caller has already pushed operands
//! (see `ir_emit::bytecode_inline_call`). FixStack stores those operands into locals above every
//! other at the opening marker and reloads them under the call's result at the closing one; an
//! object a `new` left among them is then constructed after its arguments instead. A coroutine runs
//! the same passes inside its own transformation, so only the other methods are handled here. The
//! normalized body then goes through kotlinc's optimizer like a transformed coroutine's.
//!
//! The emitted body is valid as it stands, with the operands left under the inlined code, so a body
//! the passes cannot normalize keeps them there: its markers become `nop`s.

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
    /// not a transformed coroutine.
    pub(super) fn fix_inline_call_stacks(&mut self) {
        for index in 0..self.methods.len() {
            let method = &self.methods[index];
            if method
                .rewrite_source
                .as_deref()
                .is_some_and(|source| source.transformed.is_some())
            {
                continue;
            }
            let Some(markers) = method.code.as_deref().and_then(inline_call_markers) else {
                continue;
            };
            if markers.is_empty() {
                continue;
            }
            if let Err(reason) = self.fix_inline_call_stack(index) {
                crate::trace_compiler!(
                    "bytecode",
                    "{} method #{index} keeps its operands under its inline calls: {reason}",
                    self.internal_name
                );
                let code = self.methods[index]
                    .code
                    .as_mut()
                    .expect("a method with markers has a body");
                for pc in markers {
                    code[pc] = 0x00;
                    code[pc + 1] = 0x00;
                }
            }
        }
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
