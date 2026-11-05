//! A sealed classifier's subtypes: the nest entries of its nested subclasses and, from Java 17
//! class files on, its `PermittedSubclasses` attribute.

use crate::ir::{IrClass, IrFile};
use crate::jvm::classfile::{ClassWriter, InnerClassSpec};

pub(super) fn register(cw: &mut ClassWriter, ir: &IrFile, c: &IrClass, emit_permitted: bool) {
    // The IR records subtype relationships for EVERY class; only a SEALED classifier turns them
    // into PermittedSubclasses + nest entries (a plain interface with an anonymous implementor was
    // seeding that implementor's class constant into its own pool). A nested subclass's entry
    // interns where the `InnerClasses` table is written, after `@Metadata`, unless the class body
    // names it first.
    if !c.is_sealed {
        return;
    }
    let self_identity = c.fq_name_id();
    let subs = c.sorted_sealed_subclasses();
    if subs.is_empty() {
        return;
    }
    for &sub in &subs {
        if sub != self_identity && sub.same_or_nested_within(self_identity) {
            let rendered = sub.render();
            cw.add_inner_class(InnerClassSpec {
                inner: rendered,
                outer: Some(self_identity.render()),
                name: Some(
                    sub.nested_segment_within(self_identity)
                        .expect("a nested sealed subtype must have a relative segment")
                        .to_string(),
                ),
                access: ir
                    .classes
                    .iter()
                    .find(|candidate| candidate.fq_name_id() == sub)
                    .map_or(0x0019, |candidate| {
                        crate::jvm::inner_classes::class_access(ir, candidate)
                    }),
            });
        }
    }
    if emit_permitted {
        let rendered: Vec<String> = subs.iter().map(|subclass| subclass.render()).collect();
        for sub in &rendered {
            cw.seed_class(sub);
        }
        cw.set_permitted_subclasses(rendered);
    }
}
