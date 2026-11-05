//! The anonymous objects a method of a copy constructs: kotlinc's `MethodInliner` meets each one's
//! `new` while it inlines the copy's lambdas into the method (`handleAnonymousObjectRegeneration`)
//! and regenerates it as the copy's next object, enclosed by that method. A lambda the method
//! passes to the object's constructor is inlined into the nested copy in turn, which takes the
//! lambda's captured values in its place.

use crate::jvm::class_node::ClassNode;

use super::super::class_roles::RegeneratedClass;
use super::super::name_generator::{sam_wrapper_name, NameGenerator};
use super::super::object_regeneration::{AnonymousObjects, ObjectLambda, RegeneratedObject};
use super::super::InlineError;
use super::{
    copy_sam_wrapper, regenerate, CallSite, Regeneration, RegenerationError, SamWrapperCopy,
};

/// Where the objects one method of the copy constructs are regenerated.
pub(super) struct NestedObjects<'r, 'a> {
    /// The copy whose method constructs them.
    pub regeneration: &'r Regeneration<'a>,
    pub names: &'r mut NameGenerator,
    /// The copy's own constructor descriptor, which a construction of the copy in its own method
    /// calls.
    pub own_constructor: &'r str,
    /// The method as the copy declares it, which encloses each nested copy.
    pub method: &'r str,
    pub descriptor: &'r str,
    /// Each nested original with its copy, in the order they were regenerated.
    pub renamed: Vec<(String, String)>,
    /// The nested copies, and the classes they regenerated in turn.
    pub classes: Vec<(String, Vec<u8>)>,
    /// The SAM wrappers the method and the nested copies construct.
    pub sam_wrappers: Vec<SamWrapperCopy>,
    /// Whether a nested copy still names a reified parameter of an enclosing inline function: the
    /// copy that constructs it then does too (kotlinc merges the nested transformation's
    /// `reifiedTypeParametersUsages` into the enclosing one's).
    pub reified_parameters_remain: bool,
}

impl AnonymousObjects for NestedObjects<'_, '_> {
    fn same_module(&self) -> bool {
        self.regeneration.same_module
    }

    fn regenerate_singleton(
        &mut self,
        class: &str,
        prior_classes: &[(String, String)],
    ) -> Result<RegeneratedObject, InlineError> {
        let unsupported =
            |reason| InlineError::Regeneration(RegenerationError::Unsupported(reason));
        let bytes = self
            .regeneration
            .classes
            .class_file(class)
            .ok_or(unsupported(
                "a nested object whose class file is not on the classpath",
            ))?;
        let original = ClassNode::read(&bytes)
            .map_err(|_| unsupported("a nested object class that does not read"))?;
        let constructor = super::split_constructor(&original)
            .map_err(InlineError::Regeneration)?
            .0
            .desc
            .clone();
        self.regenerate(class, &constructor, &[], &[], prior_classes)
    }

    fn regenerate(
        &mut self,
        class: &str,
        constructor_desc: &str,
        lambdas: &[ObjectLambda<'_>],
        suspend_values: &[usize],
        prior_classes: &[(String, String)],
    ) -> Result<RegeneratedObject, InlineError> {
        let unsupported =
            |reason| InlineError::Regeneration(RegenerationError::Unsupported(reason));
        let outer = self.regeneration;
        // The copy constructing itself anew is already regenerated (`isAlreadyRegenerated`).
        if class == outer.new_class {
            return Ok(RegeneratedObject {
                name: class.to_string(),
                constructor_desc: self.own_constructor.to_string(),
                reified_parameters_remain: false,
            });
        }
        let bytes = outer.classes.class_file(class).ok_or(unsupported(
            "a nested object whose class file is not on the classpath",
        ))?;
        let original = ClassNode::read(&bytes)
            .map_err(|_| unsupported("a nested object class that does not read"))?;
        // A SAM wrapper is copied, not regenerated: kotlinc replays it under the calling class's
        // name, which takes no number under the copy, and the calling class shares the copy.
        if outer.classes.regenerated_class(class) == Some(RegeneratedClass::SamWrapper) {
            let name = sam_wrapper_name(outer.calling_class, class)
                .ok_or(unsupported("a SAM wrapper without its ABI name segment"))?;
            let bytes = copy_sam_wrapper(&original, &name, outer.major)
                .map_err(InlineError::Regeneration)?;
            self.renamed.push((class.to_string(), name.clone()));
            self.sam_wrappers.push(SamWrapperCopy {
                original: class.to_string(),
                name: name.clone(),
                bytes,
            });
            return Ok(RegeneratedObject {
                name,
                constructor_desc: constructor_desc.to_string(),
                reified_parameters_remain: false,
            });
        }
        let names = self.names.next_object();
        let new_class = names.class().to_string();
        let mut prior = outer.prior_classes.to_vec();
        prior.push((outer.original.name.clone(), outer.new_class.to_string()));
        prior.extend(prior_classes.iter().cloned());
        let regenerated = regenerate(
            &Regeneration {
                original: &original,
                new_class: &new_class,
                constructor_desc,
                call_site: CallSite {
                    owner: outer.new_class,
                    method: self.method,
                    descriptor: self.descriptor,
                    // A nested copy shares its root call's scope (`RegeneratedClassContext` keeps
                    // the root's `callSiteInfo`).
                    public_inline_scope: outer.call_site.public_inline_scope,
                    source: outer.call_site.source,
                },
                prior_classes: &prior,
                lambdas,
                suspend_values,
                continuation: None,
                ..*outer
            },
            names,
        )
        .map_err(InlineError::Regeneration)?;
        self.renamed.push((class.to_string(), new_class.clone()));
        self.classes.extend(regenerated.nested);
        self.sam_wrappers.extend(regenerated.sam_wrappers);
        self.classes.push((new_class.clone(), regenerated.bytes));
        self.reified_parameters_remain |= regenerated.reified_parameters_remain;
        Ok(RegeneratedObject {
            name: new_class,
            constructor_desc: regenerated.constructor_desc,
            reified_parameters_remain: regenerated.reified_parameters_remain,
        })
    }
}
