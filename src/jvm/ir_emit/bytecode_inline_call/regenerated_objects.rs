//! The anonymous objects of an inlined classpath body, regenerated as classes of the caller's:
//! the call-site half of kotlinc's `AnonymousObjectTransformer`, which names each copy after the
//! calling function and the callee (`ClassCodegen.getRegeneratedObjectNameGenerator`) and points
//! its `EnclosingMethod` at the calling method.

use crate::fx_hash::HashMap;
use std::rc::Rc;

use super::*;
use crate::backend::BackendClassifierSource;
use crate::jvm::bytecode_passes::redundant_boxing::ValueClassDescriptors;
use crate::jvm::class_node::ClassNode;
use crate::jvm::inline::RegeneratedClass;
use crate::jvm::inliner::{
    copy_sam_wrapper, regenerate, AnonymousObjects, CallSite, CallSiteSource, ClassNameGenerators,
    InlineError, ObjectLambda, RegeneratedObject, Regeneration, RegenerationError,
};
use crate::jvm::ir_emit::signature_formatter::Wildcards;
use crate::jvm::ir_emit::JvmSignatureFormatter;
use crate::jvm::reified_arguments::ReifiedArguments;
use crate::jvm::source_map::SourceMap;

/// The ABI name of a SAM wrapper copied into `owner`, when `class` is declared to have that role.
/// The class's declaration selects the operation; its spelling is consumed only to format the
/// copied JVM class name.
fn sam_wrapper_copy_name(
    bodies: &dyn MethodBodies,
    generators: &mut ClassNameGenerators,
    owner: &str,
    class: &str,
) -> Result<Option<(String, bool)>, RegenerationError> {
    if bodies.regenerated_class(class) != Some(RegeneratedClass::SamWrapper) {
        return Ok(None);
    }
    generators
        .sam_wrapper(owner, class)
        .map(Some)
        .ok_or(RegenerationError::Unsupported(
            "a SAM wrapper without its ABI name segment",
        ))
}

/// The method whose inline calls regenerate objects: its JVM identity, the name of the Kotlin
/// function it realizes, which names the copies, and the classifiers its type arguments' signatures
/// are written from.
#[derive(Clone)]
pub(in crate::jvm::ir_emit) struct RegenerationSite<'a> {
    pub method: String,
    pub descriptor: String,
    /// The Kotlin function name the copies are numbered under. Empty when `special` is set.
    pub function: String,
    /// A constructor or another JVM special method: copies are numbered under `$special`.
    pub special: bool,
    /// The method is a non-private inline function, whose copies are public ABI too
    /// (`isInPublicInlineScope`).
    pub public_inline: bool,
    /// The class the copies are named after and enclosed by when it is not the emitting class: a
    /// lambda realized as a class of its own whose body the emitting class implements.
    pub owner: Option<String>,
    pub symbols: &'a dyn BackendClassifierSource,
}

/// Whether `function` is a property's getter or setter.
fn is_property_accessor(ir: &IrFile, function: u32) -> bool {
    ir.local_property_layouts
        .values()
        .any(|layout| match layout {
            crate::ir::IrLocalPropertyLayout::TopLevelStorage { getter, setter, .. }
            | crate::ir::IrLocalPropertyLayout::Member { getter, setter, .. } => {
                *getter == Some(function) || *setter == Some(function)
            }
            crate::ir::IrLocalPropertyLayout::TopLevelAccessor { getter, setter, .. }
            | crate::ir::IrLocalPropertyLayout::MemberExtension { getter, setter, .. } => {
                *getter == function || *setter == Some(function)
            }
        })
}

/// The JVM descriptor of the method a lambda realized as a class of its own implements: its SAM
/// method's erased descriptor, or `invoke`'s over `Object`s.
fn class_realized_method_descriptor(ir: &IrFile, function: u32) -> Option<String> {
    ir.exprs.iter().find_map(|expression| match expression {
        IrExpr::Lambda {
            impl_fn,
            arity,
            captures,
            sam,
            ..
        } if *impl_fn == function => match sam {
            Some(target) => super::super::lambda_class::sam_bootstrap_descriptors(
                ir,
                function,
                target,
                captures.len(),
            )
            .ok()
            .map(|descriptors| descriptors.erased_method),
            None => Some(format!(
                "({})Ljava/lang/Object;",
                "Ljava/lang/Object;".repeat(*arity as usize)
            )),
        },
        _ => None,
    })
}

impl<'a> RegenerationSite<'a> {
    /// The site of the function `function`, emitted as its own method under `descriptor`: a
    /// declared function, a suspend lambda's `invokeSuspend`, a lambda class's `invoke`, or a
    /// lambda's implementation method.
    /// kotlinc numbers each copy under the emitted method's name and points its `EnclosingMethod`
    /// at that method, except from a lambda's method, where it names the function the lambda is
    /// written in. An inline function's copies are its own, as kotlinc's are, and public ABI
    /// when the function is not private. This stage regenerates objects nowhere else: not in a
    /// `$DefaultImpls` copy (`own_method` false) or an accessor, whose copies kotlinc names or
    /// writes differently. A constructor is [`Self::constructor`].
    pub(in crate::jvm::ir_emit) fn of_function(
        ir: &IrFile,
        function: u32,
        own_method: bool,
        descriptor: &str,
        env: &EmitEnv<'a>,
    ) -> Option<RegenerationSite<'a>> {
        if !own_method {
            return None;
        }
        let declaration = &ir.functions[function as usize];
        let name = &declaration.name;
        // Another file's inline function, compiled here for its callers, is declared there.
        let declared = (ir.fn_decl_lines.contains_key(&function)
            && ir.fn_source_names.get(&function) == Some(name))
            || ir.foreign_inline_templates.contains(&function);
        let suspend_lambda_body = declaration
            .dispatch_receiver
            .and_then(|class| env.emit_time_machines.suspend_lambda(class))
            .is_some_and(|lambda| lambda.invoke_suspend == function);
        // A `-Xlambdas=class` lambda's body is its class's `invoke`, a method of its own.
        let class_lambda_body = declaration
            .dispatch_receiver
            .and_then(|class| ir.class_id_by_name(class))
            .and_then(|class| ir.classes.get(class as usize)?.lambda.as_ref())
            .is_some_and(|lambda| lambda.invoke == function);
        let facade = env.facade_class.render();
        let mut owner = None;
        if is_property_accessor(ir, function) {
            // An accessor's name is special: kotlinc numbers its copies under `$special`.
            return Some(Self::special(name, descriptor, env.signature_symbols));
        }
        let (method, descriptor) = if declared || suspend_lambda_body || class_lambda_body {
            (name.clone(), descriptor.to_string())
        } else if ir.local_function_spans.contains_key(&function) {
            // A local function is its lifted method.
            (name.clone(), descriptor.to_string())
        } else if let Some((class, method)) =
            super::super::lambda_class_names::class_realized_function_location(
                ir,
                function,
                &facade,
                env.lambda_modes,
            )
        {
            // A lambda realized as a class of its own: kotlinc writes the body in that class's
            // method, which encloses and names the copies.
            let descriptor = class_realized_method_descriptor(ir, function)?;
            owner = Some(class);
            (method, descriptor)
        } else if ir.lambda_origins.contains_key(&function) {
            let (_, method) = super::super::enclosure::lambda_body_enclosure(
                ir,
                env.override_results,
                function,
                &facade,
            )?;
            // A lambda with no enclosing method is written in a static initializer: a top-level,
            // object, or companion property. kotlinc encloses its copies in `<clinit>`.
            method.unwrap_or_else(|| ("<clinit>".to_string(), "()V".to_string()))
        } else {
            return None;
        };
        // Copies of a class-realized lambda's body are named after the method it implements.
        let numbered_under = if owner.is_some() {
            method.clone()
        } else {
            name.clone()
        };
        // A copy is public ABI when the code it is written in is (`isInPublicInlineScope`): a
        // non-private inline function, or a lambda class that function's ABI publishes.
        let public_inline = (ir.inline_fns.contains(&function)
            && !ir.method_visibility(function).is_private())
            || ir.classes.iter().any(|class| {
                class
                    .lambda
                    .as_ref()
                    .is_some_and(|lambda| lambda.invoke == function && lambda.public_inline_abi)
            });
        Some(RegenerationSite {
            method,
            descriptor,
            function: numbered_under,
            special: false,
            public_inline,
            owner,
            symbols: env.signature_symbols,
        })
    }

    /// The site of a constructor. kotlinc numbers its regenerated objects under `$special` and
    /// points `EnclosingMethod` at `<init>` with `descriptor`.
    pub(in crate::jvm::ir_emit) fn constructor(
        descriptor: &str,
        symbols: &'a dyn BackendClassifierSource,
    ) -> RegenerationSite<'a> {
        Self::special("<init>", descriptor, symbols)
    }

    /// The site of a JVM special method. Copies are numbered under `$special` and retain this
    /// method as their `EnclosingMethod`; unlike a source function, no Kotlin name participates in
    /// the generated class name.
    pub(in crate::jvm::ir_emit) fn special(
        method: &str,
        descriptor: &str,
        symbols: &'a dyn BackendClassifierSource,
    ) -> RegenerationSite<'a> {
        RegenerationSite {
            method: method.to_string(),
            descriptor: descriptor.to_string(),
            function: String::new(),
            special: true,
            public_inline: false,
            owner: None,
            symbols,
        }
    }

    pub(in crate::jvm::ir_emit) fn class_initializer(
        symbols: &'a dyn BackendClassifierSource,
    ) -> RegenerationSite<'a> {
        Self::special("<clinit>", "()V", symbols)
    }

    pub(in crate::jvm::ir_emit) fn is_class_initializer(&self) -> bool {
        self.special && self.method == "<clinit>" && self.descriptor == "()V"
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

    /// The names handed out so far, to hand out again after a compilation that is not written.
    pub(super) fn snapshot(&self) -> HashMap<String, ClassNameGenerators> {
        self.0.borrow().clone()
    }

    pub(super) fn restore(&self, names: HashMap<String, ClassNameGenerators>) {
        *self.0.borrow_mut() = names;
    }
}

/// The objects of one call to `callee`: each copy is named and kept as the inlining writes it.
pub(super) struct CallObjects<'a> {
    ir: &'a IrFile,
    bodies: &'a dyn MethodBodies,
    run: &'a EmitRun,
    owner: String,
    site: Option<RegenerationSite<'a>>,
    call_expression: u32,
    major: u16,
    /// The `@Metadata` stamp the caller writes: the inlining module's `mv` and pre-release flag, as
    /// kotlinc's `AnonymousObjectTransformer` rewrites the copied class's annotation with them.
    metadata_stamp: crate::jvm::classfile::MetadataStamp,
    callee: String,
    /// The caller's source map as the call's lambdas left it: their lines are lines of it.
    caller_lines: SourceMap,
    value_classes: Rc<ValueClassDescriptors>,
    reified: ReifiedArguments,
    /// The calling class's source file name.
    source_file: Option<String>,
    /// The call's line, zero-based as kotlinc's IR reports it to a copy's state machine.
    call_line: Option<u16>,
    /// The callee is a same-module function inlined from the body this pass compiled for it.
    same_module: bool,
}

impl<'a> Emitter<'a> {
    /// The objects of the call `call_expression` to `callee`, whose lambdas' lines are lines of
    /// `caller_lines`.
    pub(super) fn call_objects(
        &self,
        call_expression: u32,
        callee: &str,
        caller_lines: SourceMap,
    ) -> CallObjects<'a> {
        let reified = crate::jvm::reified_operations::splice_arguments(
            self.ir,
            call_expression,
            &self.facade,
            self.ir.recursive_type_of,
            &|ty| self.rendered_inlined_cast_target(ty),
        );
        CallObjects {
            ir: self.ir,
            bodies: self.bodies,
            run: self.run,
            owner: self.owner.clone(),
            site: self.regeneration_site.clone(),
            call_expression,
            major: self.cw.major(),
            metadata_stamp: self.metadata_stamp,
            callee: callee.to_string(),
            caller_lines,
            value_classes: self.cw.value_classes(),
            reified,
            same_module: self.ir.compiled_inline_call(call_expression).is_some(),
            source_file: self.cw.source_file_name(),
            call_line: self
                .ir
                .expr_source_lines
                .get(&call_expression)
                .and_then(|&line| u16::try_from(line.checked_sub(1)?).ok()),
        }
    }
}

impl CallObjects<'_> {
    /// Each of the callee's type parameters with its argument at this call, as the signature
    /// kotlinc's `TypeParameterMappings` writes for it (`mapTypeParameter`, in its
    /// `GENERIC_ARGUMENT` mode: `List<String>` signs `List<+String>`, the declaration-site wildcard
    /// a parameter position would drop over a final class kept).
    fn type_arguments(&self, site: &RegenerationSite<'_>) -> Option<Vec<(String, String)>> {
        let formatter = JvmSignatureFormatter::with_symbols(self.ir, site.symbols, self.run);
        let Some(arguments) = self
            .ir
            .inline_call_type_arguments
            .get(&self.call_expression)
        else {
            return Some(Vec::new());
        };
        arguments
            .iter()
            .map(|(name, ty)| Some((name.clone(), formatter.ty_at(ty, Wildcards::Generic)?)))
            .collect()
    }
}

impl AnonymousObjects for CallObjects<'_> {
    fn same_module(&self) -> bool {
        self.same_module
    }

    fn regenerate(
        &mut self,
        class: &str,
        constructor_desc: &str,
        lambdas: &[ObjectLambda<'_>],
        prior_classes: &[(String, String)],
    ) -> Result<RegeneratedObject, InlineError> {
        let unsupported =
            |reason| InlineError::Regeneration(RegenerationError::Unsupported(reason));
        let site = self
            .site
            .as_ref()
            .ok_or(unsupported("an object inlined outside a named function"))?;
        // The class the copies are named after: the emitting class, or the class a lambda body it
        // implements is realized as.
        let owner = site.owner.clone().unwrap_or_else(|| self.owner.clone());
        let type_arguments = self
            .type_arguments(site)
            .ok_or(unsupported("a type argument without a generic signature"))?;
        let bytes = self.bodies.class_file(class).ok_or(unsupported(
            "an object whose class file is not on the classpath",
        ))?;
        let original = ClassNode::read(&bytes)
            .map_err(|_| unsupported("an object class that does not read"))?;
        let mut names = self.run.regenerated_object_names.0.borrow_mut();
        let generators = names.entry(owner.clone()).or_default();
        // A SAM wrapper is copied, not regenerated: kotlinc replays it under the calling class's
        // name once and shares the copy among that class's inline calls.
        let wrapper = sam_wrapper_copy_name(self.bodies, generators, &owner, class)
            .map_err(InlineError::Regeneration)?;
        if let Some((new_class, first)) = wrapper {
            let bytes = copy_sam_wrapper(&original, &new_class, self.major)
                .map_err(InlineError::Regeneration)?;
            if first {
                self.run
                    .machine_classes
                    .borrow_mut()
                    .push((new_class.clone(), bytes));
            }
            return Ok(RegeneratedObject {
                name: new_class,
                constructor_desc: constructor_desc.to_string(),
                reified_parameters_remain: false,
            });
        }
        let kotlin_name = (!site.special).then_some(site.function.as_str());
        let object_names = generators
            .for_function(&owner, kotlin_name)
            .for_inlined_method(&self.callee)
            .next_object();
        let new_class = object_names.class().to_string();
        let regenerated = regenerate(
            &Regeneration {
                original: &original,
                new_class: &new_class,
                constructor_desc,
                call_site: CallSite {
                    owner: &owner,
                    method: &site.method,
                    descriptor: &site.descriptor,
                    public_inline_scope: site.public_inline,
                    source: CallSiteSource {
                        file: self.source_file.as_deref().unwrap_or_default(),
                        line: self.call_line,
                    },
                },
                calling_class: &self.owner,
                type_arguments: &type_arguments,
                prior_classes,
                major: self.major,
                metadata_stamp: self.metadata_stamp,
                lambdas,
                caller_lines: &self.caller_lines,
                classes: &self.bodies,
                value_classes: &self.value_classes,
                reified: &self.reified,
                continuation: None,
                same_module: self.same_module,
            },
            object_names,
        )
        .map_err(InlineError::Regeneration)?;
        let mut shared_wrappers = Vec::new();
        for wrapper in regenerated.sam_wrappers {
            let (name, first) = generators
                .sam_wrapper(&owner, &wrapper.original)
                .ok_or(unsupported("a SAM wrapper without its ABI name segment"))?;
            debug_assert_eq!(name, wrapper.name, "one name for the calling class's copy");
            if first {
                shared_wrappers.push((wrapper.name, wrapper.bytes));
            }
        }
        let mut classes = self.run.machine_classes.borrow_mut();
        classes.push((new_class.clone(), regenerated.bytes));
        classes.extend(regenerated.nested);
        classes.extend(shared_wrappers);
        Ok(RegeneratedObject {
            name: new_class,
            constructor_desc: regenerated.constructor_desc,
            reified_parameters_remain: regenerated.reified_parameters_remain,
        })
    }

    fn regenerate_singleton(
        &mut self,
        class: &str,
        prior_classes: &[(String, String)],
    ) -> Result<RegeneratedObject, InlineError> {
        let unsupported =
            |reason| InlineError::Regeneration(RegenerationError::Unsupported(reason));
        let bytes = self.bodies.class_file(class).ok_or(unsupported(
            "an object whose class file is not on the classpath",
        ))?;
        let original = ClassNode::read(&bytes)
            .map_err(|_| unsupported("an object class that does not read"))?;
        let mut constructors = original
            .methods
            .iter()
            .filter(|method| method.name == "<init>");
        let constructor = constructors
            .next()
            .ok_or(unsupported("a class without a constructor"))?;
        if constructors.next().is_some() {
            return Err(unsupported("more than one constructor"));
        }
        let descriptor = constructor.desc.clone();
        self.regenerate(class, &descriptor, &[], prior_classes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classreader::MethodCode;

    struct DeclaredRole(Option<RegeneratedClass>);

    impl MethodBodies for DeclaredRole {
        fn body(&self, _owner: &str, _name: &str, _descriptor: &str) -> Option<MethodCode> {
            None
        }

        fn regenerated_class(&self, _internal: &str) -> Option<RegeneratedClass> {
            self.0
        }
    }

    #[test]
    fn sam_copy_dispatch_uses_the_declared_role_before_formatting_its_abi_name() {
        let mut generators = ClassNameGenerators::default();
        assert_eq!(
            sam_wrapper_copy_name(
                &DeclaredRole(Some(RegeneratedClass::AnonymousObject)),
                &mut generators,
                "CallerKt",
                "LibraryKt$sam$LooksLikeAWrapper",
            ),
            Ok(None),
        );
        assert_eq!(
            sam_wrapper_copy_name(
                &DeclaredRole(Some(RegeneratedClass::SamWrapper)),
                &mut generators,
                "CallerKt",
                "LibraryKt$sam$i$java_util_function_Supplier$0",
            ),
            Ok(Some((
                "CallerKt$inlined$sam$i$java_util_function_Supplier$0".to_string(),
                true,
            ))),
        );
        assert_eq!(
            sam_wrapper_copy_name(
                &DeclaredRole(Some(RegeneratedClass::SamWrapper)),
                &mut generators,
                "CallerKt",
                "LibraryKt$missingSegment",
            ),
            Err(RegenerationError::Unsupported(
                "a SAM wrapper without its ABI name segment",
            )),
        );
    }
}
