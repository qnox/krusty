//! Templates of inline dependency functions, lowered from their serialized KLIB bodies.
//!
//! The semantic provider supplies each selected callable's record (its source name, parameters,
//! type parameters and inline modifiers) and the serialized bodies of the KLIBs it loaded. Common
//! lowering asks for a template by the opaque identity a checked call selected, and the body is
//! lowered into the calling file by the target-free KLIB body lowering.

use std::rc::Rc;

use crate::fir::ExternalCallableId;
use crate::fir_lower::{DependencyInlineTemplate, DependencyInlineTemplates};
use crate::ir::IrFile;
use crate::klib_lowering::{lower_inline_template, TemplateCallees};
use crate::libraries::SemanticPlatform;

/// Templates lowered from the serialized bodies of the platform's KLIB dependencies.
pub(super) struct KlibInlineTemplates {
    platform: Rc<dyn SemanticPlatform>,
}

impl KlibInlineTemplates {
    /// The platform's templates, when it serializes dependency bodies.
    pub(super) fn of(platform: Rc<dyn SemanticPlatform>) -> Option<Self> {
        platform.klib_dependency_body_facts()?;
        Some(Self { platform })
    }
}

impl DependencyInlineTemplates for KlibInlineTemplates {
    fn lower(
        &self,
        ir: &mut IrFile,
        target: ExternalCallableId,
    ) -> Option<DependencyInlineTemplate> {
        let realization = self.platform.external_callable(target)?;
        let bodies = self.platform.klib_dependency_body_facts()?;
        let callees = TemplateCallees {
            declarations: bodies.bodies(),
            references: bodies.references(),
            source: self.platform.as_ref(),
        };
        let Some(template) =
            lower_inline_template(ir, &realization, bodies.inline_bodies(), callees)
        else {
            crate::trace_compiler!(
                "lower",
                "inline dependency `{}` is not a KLIB declaration",
                realization.callable.name
            );
            return None;
        };
        let template = template
            .map_err(|decline| {
                crate::trace_compiler!("lower", "inline dependency template declined: {decline}");
            })
            .ok()?;
        Some(DependencyInlineTemplate {
            function: template.function,
            name: realization.callable.name.clone(),
            type_parameters: template.type_parameters,
        })
    }
}
