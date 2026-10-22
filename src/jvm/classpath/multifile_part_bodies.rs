//! The bodies a multifile facade's functions take from its parts.
//!
//! A facade (`FlowKt`) declares no function of its own: by default each of its methods only
//! forwards to the part that declares the function (`FlowKt__TransformKt`), and under
//! `-Xmultifile-parts-inherit` it has no method at all and extends its parts instead. kotlinc
//! inlines the part's body in both cases. The forwarder names that part itself, so it decides
//! which part's body a facade method has: another part may declare a file-private function with
//! the same JVM name and descriptor.

use super::Classpath;
use crate::jvm::classreader::MethodCode;
use crate::jvm::inline::{disassemble, invoked_method};
use crate::types::{type_name, TypeName};

impl Classpath {
    /// The body of `internal`'s method `name descriptor`: the part's when `internal` is a facade
    /// that forwards to or extends a part, otherwise its own. `None` when no class in the chain
    /// declares one, or when the part a forwarder names has no readable body.
    pub(super) fn method_code_through_parts(
        &self,
        internal: TypeName,
        name: &str,
        descriptor: &str,
        catalog_complete: bool,
    ) -> Option<MethodCode> {
        let class = self.find_name(super::super::jvm_class_map::to_jvm_type_name(internal));
        let own = self.own_method_code(internal, name, descriptor, catalog_complete);
        if let Some(own) = own {
            let parts = class
                .as_ref()
                .map(|class| class.meta.multifile_parts.as_slice())
                .unwrap_or_default();
            // A forwarder names the implementation: its part's body, or none when that body is
            // unavailable. The forwarder itself is never the function's body.
            return match forwarded_part(&own, name, descriptor, parts) {
                Some(part) => {
                    self.own_method_code(type_name(part), name, descriptor, catalog_complete)
                }
                None => Some(own),
            };
        }
        let mut superclass = class.and_then(|class| class.super_class);
        while let Some(current) = superclass {
            if current == crate::types::wk::java_object() {
                break;
            }
            if let Some(body) = self.own_method_code(current, name, descriptor, catalog_complete) {
                return Some(body);
            }
            superclass = self.find_name(current).and_then(|class| class.super_class);
        }
        None
    }
}

/// The part among `parts` that the facade method `own` forwards `name descriptor` to.
fn forwarded_part<'p>(
    own: &MethodCode,
    name: &str,
    descriptor: &str,
    parts: &'p [String],
) -> Option<&'p str> {
    if parts.is_empty() {
        return None;
    }
    let instructions = disassemble(&own.code)?;
    instructions.iter().find_map(|instruction| {
        let (owner, callee, callee_descriptor, _) = invoked_method(instruction, &own.source_cp)?;
        if callee != name || callee_descriptor != descriptor {
            return None;
        }
        parts
            .iter()
            .find(|part| part.as_str() == owner)
            .map(String::as_str)
    })
}
