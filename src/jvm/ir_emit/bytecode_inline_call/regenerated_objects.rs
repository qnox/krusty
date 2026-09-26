//! The anonymous objects of an inlined classpath body, regenerated as classes of the caller's:
//! the call-site half of kotlinc's `AnonymousObjectTransformer`, which names each copy after the
//! calling function and the callee (`ClassCodegen.getRegeneratedObjectNameGenerator`) and points
//! its `EnclosingMethod` at the calling method.

use std::collections::HashMap;

use super::*;
use crate::backend::BackendClassifierSource;
use crate::jvm::class_node::ClassNode;
use crate::jvm::inliner::{
    regenerate, AnonymousObjects, CallSite, ClassNameGenerators, InlineError, Regeneration,
    RegenerationError,
};
use crate::jvm::ir_emit::JvmSignatureFormatter;

/// The method whose inline calls regenerate objects: its JVM identity, the name of the Kotlin
/// function it realizes, which names the copies, and the classifiers its type arguments' signatures
/// are written from.
#[derive(Clone)]
pub(in crate::jvm::ir_emit) struct RegenerationSite<'a> {
    pub method: String,
    pub descriptor: String,
    pub function: String,
    pub symbols: &'a dyn BackendClassifierSource,
}

impl<'a> RegenerationSite<'a> {
    /// The site of the declared function `function`, emitted as its own method under `descriptor`.
    /// This stage regenerates objects only there: not in a `$DefaultImpls` copy (`own_method`
    /// false), a constructor, an accessor, a lambda, a suspend function, or an inline function,
    /// whose copies kotlinc names or writes differently.
    pub(in crate::jvm::ir_emit) fn of_function(
        ir: &IrFile,
        function: u32,
        own_method: bool,
        descriptor: &str,
        symbols: &'a dyn BackendClassifierSource,
    ) -> Option<RegenerationSite<'a>> {
        let name = &ir.functions[function as usize].name;
        let declared = ir.fn_decl_lines.contains_key(&function)
            && ir.fn_source_names.get(&function) == Some(name);
        (own_method
            && declared
            && !ir.suspend_funs.contains(&function)
            && !ir.inline_fns.contains(&function))
        .then(|| RegenerationSite {
            method: name.clone(),
            descriptor: descriptor.to_string(),
            function: name.clone(),
            symbols,
        })
    }
}

/// The per-class name generators of one emit run, by class.
#[derive(Default)]
pub(in crate::jvm::ir_emit) struct RegeneratedObjectNames(
    std::cell::RefCell<HashMap<String, ClassNameGenerators>>,
);

impl RegeneratedObjectNames {
    /// Forget every name handed out: a discarded emit pass must not number the next one's copies.
    pub(in crate::jvm::ir_emit) fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// The objects of one call to `callee`. The first inlining of a body only settles whether the port
/// covers it (`commit == false`): it names and writes each copy without keeping either, so the
/// inlining that is written numbers its copies from the same point.
pub(super) struct CallObjects<'a> {
    ir: &'a IrFile,
    bodies: &'a dyn MethodBodies,
    run: &'a EmitRun,
    owner: String,
    site: Option<RegenerationSite<'a>>,
    call_expression: u32,
    major: u16,
    callee: String,
    commit: bool,
}

impl<'a> Emitter<'a> {
    pub(super) fn call_objects(
        &self,
        call_expression: u32,
        callee: &str,
        commit: bool,
    ) -> CallObjects<'a> {
        CallObjects {
            ir: self.ir,
            bodies: self.bodies,
            run: self.run,
            owner: self.owner.clone(),
            site: self.regeneration_site.clone(),
            call_expression,
            major: self.cw.major(),
            callee: callee.to_string(),
            commit,
        }
    }
}

/// The `@Metadata` version this compiler writes.
const METADATA_VERSION: [i32; 3] = [2, 4, 0];

impl CallObjects<'_> {
    /// Each of the callee's type parameters with its argument at this call, as the signature
    /// kotlinc's `TypeParameterMappings` writes for it (`mapTypeParameter`).
    fn type_arguments(&self, site: &RegenerationSite<'_>) -> Option<Vec<(String, String)>> {
        let formatter = JvmSignatureFormatter::with_symbols(self.ir, site.symbols, self.run);
        let Some(arguments) = self.ir.reified_call_subst.get(&self.call_expression) else {
            return Some(Vec::new());
        };
        arguments
            .iter()
            .map(|(name, ty)| Some((name.clone(), formatter.ty(ty)?)))
            .collect()
    }
}

impl AnonymousObjects for CallObjects<'_> {
    fn regenerate(
        &mut self,
        class: &str,
        constructor_desc: &str,
    ) -> Result<(String, String), InlineError> {
        let unsupported =
            |reason| InlineError::Regeneration(RegenerationError::Unsupported(reason));
        let site = self
            .site
            .as_ref()
            .ok_or(unsupported("an object inlined outside a named function"))?;
        let type_arguments = self
            .type_arguments(site)
            .ok_or(unsupported("a type argument without a generic signature"))?;
        let bytes = self.bodies.class_file(class).ok_or(unsupported(
            "an object whose class file is not on the classpath",
        ))?;
        let original = ClassNode::read(&bytes)
            .map_err(|_| unsupported("an object class that does not read"))?;
        let mut names = self.run.regenerated_object_names.0.borrow_mut();
        let generators = names.entry(self.owner.clone()).or_default();
        let mut scratch;
        let generators = if self.commit {
            generators
        } else {
            scratch = generators.clone();
            &mut scratch
        };
        let new_class = generators
            .for_function(&self.owner, Some(&site.function))
            .for_inlined_method(&self.callee)
            .next_object()
            .class()
            .to_string();
        let regenerated = regenerate(&Regeneration {
            original: &original,
            new_class: &new_class,
            constructor_desc,
            call_site: CallSite {
                owner: &self.owner,
                method: &site.method,
                descriptor: &site.descriptor,
                public_inline_scope: false,
            },
            type_arguments: &type_arguments,
            major: self.major,
            metadata_version: &METADATA_VERSION,
        })
        .map_err(InlineError::Regeneration)?;
        if self.commit {
            self.run
                .machine_classes
                .borrow_mut()
                .push((new_class.clone(), regenerated.bytes));
        }
        Ok((new_class, regenerated.constructor_desc))
    }
}
