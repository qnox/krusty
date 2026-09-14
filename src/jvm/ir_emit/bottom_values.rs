//! JVM realization of the common IR's bottom-value completion contract.

use crate::jvm::classfile::{ClassWriter, CodeBuilder};

/// Discard the value a producer physically left above `baseline`, then optionally make the path
/// visibly divergent to the verifier. The producer's own JVM emission owns its stack effect; common
/// IR deliberately carries no word count or descriptor.
pub(super) fn finish(cw: &mut ClassWriter, code: &mut CodeBuilder, baseline: i32, diverge: bool) {
    if !code.can_fall_through() {
        return;
    }
    match code.stack_height() - baseline {
        0 => {}
        1 => code.pop(),
        2 => code.pop2(),
        words => {
            panic!("bottom producer left an unsupported physical stack delta of {words} words")
        }
    }
    if !diverge {
        return;
    }
    let class = cw.class_ref("kotlin/KotlinNothingValueException");
    code.new_obj(class);
    code.dup();
    let constructor = cw.methodref("kotlin/KotlinNothingValueException", "<init>", "()V");
    code.invokespecial(constructor, 0, 0);
    code.athrow();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_word_unreachable_producer_needs_no_second_termination() {
        let mut writer = ClassWriter::new("BottomCompletion", "java/lang/Object");
        let mut code = CodeBuilder::new(0);
        let baseline = code.stack_height();
        code.splice_inline(&[0x01, 0xbf], &[], 1, 0, 0, 0, false);
        let terminated = code.bytes.clone();

        finish(&mut writer, &mut code, baseline, true);

        assert_eq!(code.bytes, terminated);
        assert!(!code.can_fall_through());
    }

    #[test]
    fn a_reachable_zero_word_producer_uses_the_selected_completion() {
        let mut writer = ClassWriter::new("BottomCompletion", "java/lang/Object");
        let mut fallthrough = CodeBuilder::new(0);
        finish(&mut writer, &mut fallthrough, 0, false);
        assert!(fallthrough.bytes.is_empty());
        assert!(fallthrough.can_fall_through());

        let mut divergent = CodeBuilder::new(0);
        finish(&mut writer, &mut divergent, 0, true);
        assert!(!divergent.bytes.is_empty());
        assert!(!divergent.can_fall_through());
    }

    #[test]
    fn a_branchy_terminal_splice_keeps_the_dead_stack_at_its_baseline() {
        let mut code = CodeBuilder::new(0);
        // `ifnull left; throw null; left: throw null`: representative branch-bearing bulk bytes
        // whose transformed CFG has no caller continuation. Its selected result width is therefore
        // zero even if its descriptor declared a reference result before transformation.
        code.splice_inline(
            &[0x01, 0xc6, 0x00, 0x05, 0x01, 0xbf, 0x01, 0xbf],
            &[],
            1,
            0,
            0,
            0,
            false,
        );

        assert_eq!(code.stack_height(), 0);
        assert!(!code.can_fall_through());
    }
}
