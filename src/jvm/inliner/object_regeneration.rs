//! The anonymous objects an inlined body constructs, regenerated for the call site: kotlinc's
//! `MethodInliner` hands each one to an `AnonymousObjectTransformer` as it meets the object's `new`
//! (`handleAnonymousObjectRegeneration`) and renames the body's references to the copy.

use crate::jvm::method_node::{Insn, MethodNode, Node};

use super::anonymous_object::{MalformedType, TypeRemapper};
use super::callee_shape::is_anonymous_class;
use super::InlineError;
use super::RegenerationError;

const NEW: u8 = 0xbb;
const INVOKESPECIAL: u8 = 0xb7;

/// Where the objects of one inlined call are regenerated.
pub(crate) trait AnonymousObjects {
    /// Regenerate `class`, which the body constructs through `constructor_desc`, for this call:
    /// the copy's name and the descriptor of its constructor.
    fn regenerate(
        &mut self,
        class: &str,
        constructor_desc: &str,
    ) -> Result<(String, String), InlineError>;
}

/// Regenerate every anonymous object `node` constructs and rename the references to it. Each
/// `new` takes a fresh copy, paired with the constructor call it reaches in order, as kotlinc pairs
/// its transformation infos with the `new`s it visits; every later reference names the latest copy.
pub(super) fn regenerate_objects(
    node: &mut MethodNode,
    objects: &mut dyn AnonymousObjects,
) -> Result<(), InlineError> {
    let mut constructors = node
        .instructions()
        .filter_map(|insn| match insn {
            Insn::Method {
                op: INVOKESPECIAL,
                owner,
                name,
                desc,
                ..
            } if name == "<init>" && is_anonymous_class(owner) => Some(desc.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter();
    let mut remapper = TypeRemapper::default();
    // The copy's constructor descriptor, by the copy's name.
    let mut new_constructors: Vec<(String, String)> = Vec::new();
    for entry in &mut node.nodes {
        let Node::Insn(insn) = entry else { continue };
        if let Insn::Type { op: NEW, class } = insn {
            if is_anonymous_class(class) {
                let constructor = constructors
                    .next()
                    .ok_or(InlineError::UnpairedAnonymousObject)?;
                let (name, desc) = objects.regenerate(class, &constructor)?;
                remapper.add_mapping(class, &name);
                new_constructors.push((name, desc));
            }
        }
        if let Insn::Method {
            op: INVOKESPECIAL,
            owner,
            name,
            desc,
            ..
        } = insn
        {
            if name == "<init>" && is_anonymous_class(owner) {
                let renamed = remapper.map(owner);
                let (_, new_desc) = new_constructors
                    .iter()
                    .rev()
                    .find(|(copy, _)| *copy == renamed)
                    .ok_or(InlineError::UnpairedAnonymousObject)?;
                *owner = renamed;
                *desc = new_desc.clone();
                continue;
            }
        }
        remapper.remap_insn(insn).map_err(malformed)?;
    }
    for block in &mut node.try_catch_blocks {
        if let Some(class) = &mut block.catch_type {
            *class = remapper.map_type(class).map_err(malformed)?;
        }
    }
    for local in &mut node.local_variables {
        local.desc = remapper.map_desc(&local.desc).map_err(malformed)?;
    }
    Ok(())
}

fn malformed(malformed: MalformedType) -> InlineError {
    InlineError::Regeneration(RegenerationError::Malformed(malformed))
}
