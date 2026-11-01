//! A dependency inline function's compiled body normalized into a [`DependencyInlineTemplate`].
//!
//! The body is decoded into typed blocks (`jvm::inline_template`), its control flow structured,
//! and its operations rebuilt as template expressions whose calls are the exact declarations the
//! provider normalizes their JVM targets to. See "JVM dependency inline templates" in
//! `docs/IMPLEMENTATION_PLAN.md`.

mod calls;
mod control;
mod emit;

use super::JvmLibraries;
use crate::jvm::inline_template::decode;
use crate::jvm::method_node::MethodNode;
use crate::libraries::{DependencyInlineTemplate, LibraryCallable, TemplateDecline};

const ACC_STATIC: u16 = 0x0008;

impl JvmLibraries {
    /// The template of the inline function `callable`, normalized from its compiled body in this
    /// provider's class files.
    ///
    /// Declines a declaration that is not inline, is `suspend`, has context parameters, reified
    /// type parameters, or physical parameters other than its semantic ones; a body with an
    /// exception table, a switch or irreducible flow; and any instruction or call the template
    /// form does not express.
    pub fn dependency_inline_template(
        &self,
        callable: &LibraryCallable,
    ) -> Result<DependencyInlineTemplate, TemplateDecline> {
        if !callable.inline.can_inline() {
            return Err(TemplateDecline::NoBody);
        }
        if callable.suspend {
            return Err(TemplateDecline::UnsupportedDeclaration(
                "a suspend function",
            ));
        }
        if callable.context_count != 0 {
            return Err(TemplateDecline::UnsupportedDeclaration(
                "context parameters",
            ));
        }
        if !callable.reified_type_parameter_ordinals.is_empty() {
            return Err(TemplateDecline::Reified);
        }
        if callable.params.len() != callable.physical_params.len() {
            return Err(TemplateDecline::UnsupportedDeclaration(
                "physical parameters other than the semantic ones",
            ));
        }
        let code = self
            .cp
            .method_code_name(
                callable.owner,
                callable.physical_name(),
                &callable.descriptor,
            )
            .ok_or(TemplateDecline::NoBody)?;
        let method = MethodNode::read(
            ACC_STATIC,
            callable.physical_name(),
            &callable.descriptor,
            &code,
        )
        .map_err(|_| TemplateDecline::Undecodable)?;
        // The decoder hands the owner to the frame analysis as a class-file name.
        let body =
            decode(&callable.owner.render(), &method).map_err(|_| TemplateDecline::Undecodable)?;
        if !body.handlers.is_empty() {
            return Err(TemplateDecline::ExceptionHandlers);
        }
        let shapes = control::structure(&body)?;
        emit::Emitter::new(self, callable, &body).finish(&shapes)
    }
}

#[cfg(test)]
mod tests;
