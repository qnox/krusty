//! The serialized IR bodies of the declarations a set of KLIB libraries publishes.
//!
//! A selected dependency callable reaches its body through the exact serialized identity its
//! provider published with it (`KlibDependencyCallable`), never through a name, an owner, or a
//! parameter tuple. A public signature is unique only inside one KLIB, so every body key includes
//! the declaring module's manifest `unique_name`. Calls inside a serialized body recover that
//! module only from the producer's manifest dependency graph; current-compilation input roles and
//! unrelated declarations are not linkage evidence.

use std::collections::VecDeque;

use crate::fx_hash::{HashMap, HashSet};

use crate::klib::KlibManifest;
use crate::libraries::{KlibDeclarationSignature, KlibDependencyCallable};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrBody, KlibIrExprKind, KlibIrFunction, KlibIrMember, KlibIrStatement,
    KlibIrType, KlibIrTypeOperator,
};
use crate::metadata::klib_ir::{KlibIrModuleTrees, KlibIrSignature, KlibIrSymbolKind};
use crate::types::{type_name_child, TypeName};

/// The decoded function declarations of a set of KLIB libraries, joined by exact signature.
pub struct KlibDeclarationBodies {
    libraries: Vec<KlibIrModuleTrees>,
    modules: HashMap<String, usize>,
    /// Which library defines each linkable identity.
    owners: HashMap<(String, KlibIrSignature), usize>,
    /// The producer-declared module graph. These are manifest `depends` edges, not the roles the
    /// current compilation assigned its inputs.
    dependencies: HashMap<String, Vec<String>>,
}

impl Default for KlibDeclarationBodies {
    fn default() -> Self {
        Self {
            libraries: Vec::new(),
            modules: HashMap::default(),
            owners: HashMap::default(),
            dependencies: HashMap::default(),
        }
    }
}

/// One decoded KLIB together with the producer graph recorded in its manifest.
pub struct KlibBodyLibrary {
    module: String,
    dependencies: Vec<String>,
    trees: KlibIrModuleTrees,
}

impl KlibBodyLibrary {
    /// Pair decoded trees with the identity and producer edges recorded by their archive.
    /// A manifest without `unique_name` cannot provide stable cross-archive identity.
    pub fn from_manifest(manifest: &KlibManifest, trees: KlibIrModuleTrees) -> Option<Self> {
        Some(Self::over(
            manifest.unique_name()?.to_owned(),
            manifest.depends().into_iter().map(str::to_owned),
            trees,
        ))
    }

    #[cfg(test)]
    pub(crate) fn new(
        module: String,
        dependencies: impl IntoIterator<Item = String>,
        trees: KlibIrModuleTrees,
    ) -> Self {
        Self::over(module, dependencies, trees)
    }

    fn over(
        module: String,
        dependencies: impl IntoIterator<Item = String>,
        trees: KlibIrModuleTrees,
    ) -> Self {
        Self {
            module,
            dependencies: dependencies.into_iter().collect(),
            trees,
        }
    }
}

/// Exact origin of a serialized body call in the producer's dependency graph.
pub(crate) enum KlibCalleeOrigin {
    Exact(KlibDependencyCallable),
    Missing,
    Ambiguous,
}

/// Two library inputs claim one module identity, or duplicate one declaration inside it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibDeclarationBodiesError {
    module: String,
    signature: Option<KlibIrSignature>,
    libraries: [usize; 2],
}

impl KlibDeclarationBodiesError {
    pub fn module(&self) -> &str {
        &self.module
    }

    /// The exact duplicated declaration, or `None` when two archives claim the module itself.
    pub fn signature(&self) -> Option<&KlibIrSignature> {
        self.signature.as_ref()
    }

    /// The positions, in input order, of the two conflicting libraries.
    pub fn libraries(&self) -> [usize; 2] {
        self.libraries
    }
}

impl std::fmt::Display for KlibDeclarationBodiesError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [first, second] = self.libraries;
        match &self.signature {
            Some(signature) => write!(
                formatter,
                "KLIB libraries {first} and {second} both define the declaration {signature:?} in module {}",
                self.module
            ),
            None => write!(
                formatter,
                "KLIB libraries {first} and {second} both claim module {}",
                self.module
            ),
        }
    }
}

impl std::error::Error for KlibDeclarationBodiesError {}

impl KlibDeclarationBodies {
    /// Index each named module's decoded trees. Every module occurs once; unrelated modules may
    /// legally publish equal internal signatures.
    #[cfg(test)]
    pub(crate) fn from_libraries(
        libraries: Vec<(String, KlibIrModuleTrees)>,
    ) -> Result<Self, KlibDeclarationBodiesError> {
        Self::from_body_libraries(
            libraries
                .into_iter()
                .map(|(module, trees)| KlibBodyLibrary::over(module, Vec::new(), trees))
                .collect(),
        )
    }

    /// Index decoded libraries with their producer-declared manifest dependency edges.
    ///
    /// An empty edge set stays empty: manifests which omit `depends` provide no authority to
    /// search every input for an equal bare signature.
    pub fn from_body_libraries(
        libraries: Vec<KlibBodyLibrary>,
    ) -> Result<Self, KlibDeclarationBodiesError> {
        let mut owners = HashMap::default();
        let mut modules = HashMap::default();
        let mut duplicate_module = None;
        let mut dependencies = HashMap::<String, HashSet<String>>::default();
        for (library, input) in libraries.iter().enumerate() {
            if let Some(previous) = modules.insert(input.module.clone(), library) {
                duplicate_module.get_or_insert((input.module.clone(), [previous, library]));
            }
            dependencies
                .entry(input.module.clone())
                .or_default()
                .extend(input.dependencies.iter().cloned());
            for signature in input.trees.function_signatures() {
                let identity = (input.module.clone(), signature.clone());
                if let Some(previous) = owners.insert(identity, library) {
                    return Err(KlibDeclarationBodiesError {
                        module: input.module.clone(),
                        signature: Some(signature.clone()),
                        libraries: [previous, library],
                    });
                }
            }
        }
        if let Some((module, libraries)) = duplicate_module {
            return Err(KlibDeclarationBodiesError {
                module,
                signature: None,
                libraries,
            });
        }
        Ok(Self {
            libraries: libraries.into_iter().map(|input| input.trees).collect(),
            modules,
            owners,
            dependencies: dependencies
                .into_iter()
                .map(|(module, dependencies)| (module, dependencies.into_iter().collect()))
                .collect(),
        })
    }

    /// Whether exactly one producer-reachable module serializes `classifier` with no
    /// object-initialization work. The checked receiver separately proves that this exact classifier
    /// is a singleton. This body-side proof admits only an `Any`-delegating empty constructor,
    /// literal field initialization and an empty anonymous initializer. An absent, multiply owned,
    /// inherited, or executable initializer grants no authority.
    pub(crate) fn singleton_initialization_is_inert(
        &self,
        module: &str,
        classifier: TypeName,
    ) -> bool {
        let mut found = None;
        for module in self.body_provider_modules(module) {
            let Some(&library) = self.modules.get(&module) else {
                continue;
            };
            let mut in_module = None;
            for tree in self.libraries[library].trees() {
                find_class(&tree.arena, &tree.declaration, classifier, &mut in_module);
                if in_module == Some(false) {
                    return false;
                }
            }
            if let Some(inert) = in_module {
                if found.replace(inert).is_some() {
                    return false;
                }
            }
        }
        found == Some(true)
    }

    /// Modules whose bodies the producer graph rooted at `module` can publish.
    ///
    /// This is the same manifest-edge closure used to resolve serialized calls. It deliberately
    /// ignores unrelated current-compilation inputs: those archives cannot contribute a callee to
    /// a body rooted in the selected platform stdlib.
    pub(crate) fn body_provider_modules(&self, module: &str) -> HashSet<String> {
        let mut pending = VecDeque::from([module.to_owned()]);
        let mut reachable = HashSet::default();
        while let Some(module) = pending.pop_front() {
            if !reachable.insert(module.clone()) {
                continue;
            }
            if let Some(dependencies) = self.dependencies.get(&module) {
                pending.extend(dependencies.iter().cloned());
            }
        }
        reachable
    }

    /// Resolve a call symbol in the graph of the KLIB body which serialized it.
    ///
    /// A definition in the body module wins. Otherwise exactly one reachable producer module
    /// must define the signature. Input order and equal declarations outside that graph have no
    /// bearing on the answer.
    pub(crate) fn callee_origin(
        &self,
        body_module: &str,
        declaration: &KlibDeclarationSignature,
    ) -> KlibCalleeOrigin {
        let signature = ir_signature(declaration);
        if self
            .owners
            .contains_key(&(body_module.to_owned(), signature.clone()))
        {
            return KlibCalleeOrigin::Exact(KlibDependencyCallable {
                module: body_module.to_owned(),
                declaration: declaration.clone(),
            });
        }

        let mut pending = self
            .dependencies
            .get(body_module)
            .into_iter()
            .flatten()
            .cloned()
            .collect::<VecDeque<_>>();
        let mut visited = HashSet::default();
        let mut owner = None;
        while let Some(module) = pending.pop_front() {
            if !visited.insert(module.clone()) {
                continue;
            }
            if self
                .owners
                .contains_key(&(module.clone(), signature.clone()))
            {
                if owner.replace(module.clone()).is_some() {
                    return KlibCalleeOrigin::Ambiguous;
                }
            }
            if let Some(dependencies) = self.dependencies.get(&module) {
                pending.extend(dependencies.iter().cloned());
            }
        }
        match owner {
            Some(module) => KlibCalleeOrigin::Exact(KlibDependencyCallable {
                module,
                declaration: declaration.clone(),
            }),
            None => KlibCalleeOrigin::Missing,
        }
    }

    /// The function declared under exactly `signature`, with the arena that owns its body.
    pub fn function(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<(&KlibIrArena, &KlibIrFunction)> {
        let signature = ir_signature(&identity.declaration);
        let library = *self
            .owners
            .get(&(identity.module.clone(), signature.clone()))?;
        self.libraries[library].function(&signature)
    }

    /// Normalize the exact inline body `return value is T` (or `!is`) into a call-site plan.
    /// Every join is by serialized symbol identity. Any extra statement, conversion, nullable
    /// wrapper, or different return target leaves the declaration without this narrow plan.
    pub(crate) fn reified_type_test_inline_plan(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Option<crate::libraries::InlineBodyPlan> {
        let (arena, function) = self.function(identity)?;
        let KlibIrBody::Block(statements) = function.body.as_ref()? else {
            return None;
        };
        let [KlibIrStatement::Expression(returned)] = statements.as_slice() else {
            return None;
        };
        let KlibIrExprKind::Return { target, value } = &arena.expr(*returned).kind else {
            return None;
        };
        if target != &function.base.symbol {
            return None;
        }
        let KlibIrExprKind::TypeOperator {
            operator,
            operand,
            argument,
        } = &arena.expr(*value).kind
        else {
            return None;
        };
        let negated = match operator {
            KlibIrTypeOperator::InstanceOf => false,
            KlibIrTypeOperator::NotInstanceOf => true,
            _ => return None,
        };
        let KlibIrType::Simple {
            classifier,
            nullability: crate::metadata::klib_ir::tree::KlibIrNullability::NotSpecified,
            arguments,
            abbreviation: None,
            ..
        } = arena.ty(*operand)
        else {
            return None;
        };
        if !arguments.is_empty() {
            return None;
        }
        let type_parameter = function
            .type_parameters
            .iter()
            .position(|parameter| parameter.base.symbol == *classifier)?;
        let KlibIrExprKind::GetValue { symbol, .. } = &arena.expr(*argument).kind else {
            return None;
        };
        let parameters = function
            .context_parameters
            .iter()
            .chain(function.extension_receiver.iter())
            .chain(&function.regular_parameters);
        let value_parameter = parameters
            .clone()
            .position(|parameter| parameter.base.symbol == *symbol)?;
        let mut defaults = Vec::new();
        for (parameter, declaration) in parameters.enumerate() {
            let Some(value) = declaration.default_value else {
                continue;
            };
            if !matches!(
                arena.expr(value).kind,
                KlibIrExprKind::Const(crate::metadata::klib_ir::KlibIrConstant::Null)
            ) {
                return None;
            }
            defaults.push(crate::libraries::InlineBodyDefault {
                parameter,
                value: crate::libraries::DefaultValue::Null,
            });
        }
        Some(crate::libraries::InlineBodyPlan::ReifiedTypeTest {
            value: crate::libraries::InlineBodyValue::Parameter(value_parameter),
            type_parameter: u32::try_from(type_parameter).ok()?,
            negated,
            defaults,
        })
    }

    /// Normalize an exact `@InlineOnly` body which either returns one direct invocation of a
    /// function parameter, or evaluates that invocation and then returns one of its other
    /// parameters (`also`/`apply` return their receiver). Metadata has already proved which
    /// parameter is an in-caller-frame lambda; the IR proves the invocation receiver, every
    /// argument and the returned value by declaration-owned parameter symbols. A cast, temporary,
    /// extra statement, omitted operand or non-parameter operand rejects the shape instead of
    /// recovering an invocation from a callable spelling.
    pub(crate) fn direct_lambda_invocation_inline_plan(
        &self,
        identity: &KlibDependencyCallable,
        declaration: &crate::metadata::semantic::KotlinFunction,
    ) -> Option<crate::libraries::InlineBodyPlan> {
        let (arena, function) = self.function(identity)?;
        if function.dispatch_receiver.is_some() || declaration.param_defaults.contains(&true) {
            return None;
        }
        let KlibIrBody::Block(statements) = function.body.as_ref()? else {
            return None;
        };
        // `let`/`run`/`with` return the invocation itself; `also`/`apply` evaluate it as a
        // statement and then return one of their own parameters.
        let returned_value = |statement: crate::metadata::klib_ir::tree::KlibIrExprId| match &arena
            .expr(statement)
            .kind
        {
            KlibIrExprKind::Return { target, value } if target == &function.base.symbol => {
                Some(*value)
            }
            _ => None,
        };
        let (invocation, returned_symbol) = match statements.as_slice() {
            [KlibIrStatement::Expression(returned)] => (returned_value(*returned)?, None),
            [KlibIrStatement::Expression(invocation), KlibIrStatement::Expression(returned)] => {
                let KlibIrExprKind::GetValue { symbol, .. } =
                    &arena.expr(returned_value(*returned)?).kind
                else {
                    return None;
                };
                (*invocation, Some(symbol))
            }
            _ => return None,
        };
        let KlibIrExprKind::Call {
            access,
            super_qualifier: None,
        } = &arena.expr(invocation).kind
        else {
            return None;
        };
        if !is_direct_lambda_invocation(access) {
            return None;
        }
        let operands = match &access.arguments {
            crate::metadata::klib_ir::tree::KlibIrArguments::Flat(arguments) => {
                arguments.iter().copied().collect::<Option<Vec<_>>>()?
            }
            crate::metadata::klib_ir::tree::KlibIrArguments::Split {
                dispatch_receiver: Some(dispatch),
                extension_receiver,
                values,
            } => std::iter::once(*dispatch)
                .chain(*extension_receiver)
                .chain(values.iter().copied().collect::<Option<Vec<_>>>()?)
                .collect(),
            crate::metadata::klib_ir::tree::KlibIrArguments::Split {
                dispatch_receiver: None,
                ..
            } => return None,
        };
        let parameters = function
            .context_parameters
            .iter()
            .chain(function.extension_receiver.iter())
            .chain(&function.regular_parameters)
            .collect::<Vec<_>>();
        #[derive(Clone, Copy)]
        enum Operand {
            Receiver,
            Parameter(usize),
        }
        let receiver_parameter = declaration
            .receiver
            .as_ref()
            .map(|_| declaration.context_count);
        let semantic_operand = |ir_parameter: usize| {
            if receiver_parameter == Some(ir_parameter) {
                Some(Operand::Receiver)
            } else {
                Some(Operand::Parameter(ir_parameter.checked_sub(
                    usize::from(receiver_parameter.is_some_and(|receiver| ir_parameter > receiver)),
                )?))
            }
        };
        // Inline plans use one virtual parameter coordinate which inserts the extension receiver
        // after context parameters. Translate through semantic operands first rather than assuming
        // the serialized IR parameter order is itself that public contract.
        let plan_parameter = |parameter| {
            parameter
                + usize::from(receiver_parameter.is_some_and(|receiver| parameter >= receiver))
        };
        let plan_operand = |operand| {
            Some(match operand {
                Operand::Receiver => {
                    crate::libraries::InlineBodyValue::Parameter(receiver_parameter?)
                }
                Operand::Parameter(parameter) => {
                    crate::libraries::InlineBodyValue::Parameter(plan_parameter(parameter))
                }
            })
        };
        let parameter_operand = |symbol: &crate::metadata::klib_ir::KlibIrSymbol| {
            parameters
                .iter()
                .position(|parameter| parameter.base.symbol == *symbol)
                .and_then(semantic_operand)
        };
        let operands = operands
            .iter()
            .map(|operand| {
                let KlibIrExprKind::GetValue { symbol, .. } = &arena.expr(*operand).kind else {
                    return None;
                };
                parameter_operand(symbol)
            })
            .collect::<Option<Vec<_>>>()?;
        let (&Operand::Parameter(lambda_parameter), arguments) = operands.split_first()? else {
            return None;
        };
        // The returned parameter is a value the caller supplied, never the inlined lambda itself.
        let result = match returned_symbol.map(parameter_operand) {
            None => None,
            Some(Some(Operand::Parameter(parameter))) if parameter == lambda_parameter => {
                return None;
            }
            Some(operand) => Some(plan_operand(operand?)?),
        };
        let metadata_parameter = lambda_parameter;
        let modifier = *declaration.inline_modifiers.get(metadata_parameter)?;
        if !modifier.runs_in_caller_frame() {
            return None;
        }
        let crate::metadata::semantic::KotlinType::Class {
            internal,
            args,
            nullable: false,
            shape,
        } = declaration.params.get(metadata_parameter)?
        else {
            return None;
        };
        if !crate::metadata::semantic::is_kotlin_function_classifier(internal)
            || shape.suspend
            || args.len().checked_sub(1)? != arguments.len()
        {
            return None;
        }
        Some(crate::libraries::InlineBodyPlan::InvokeLambda {
            frame: crate::libraries::InlineBodyFrame {
                callee: declaration.name.clone().into_boxed_str(),
                inline_only: true,
                source: None,
            },
            lambda_parameter: plan_parameter(lambda_parameter),
            arguments: arguments
                .iter()
                .copied()
                .map(plan_operand)
                .collect::<Option<Vec<_>>>()?,
            prologue: Vec::new(),
            cleanup: Vec::new(),
            cause: None,
            recovery: None,
            defaults: Vec::new(),
            result,
        })
    }

    /// Whether the exact serialized declaration intentionally carries an executable body.
    pub(crate) fn function_has_body(&self, identity: &KlibDependencyCallable) -> Option<bool> {
        self.function(identity)
            .map(|(_, function)| function.body.is_some())
    }

    /// Exact producer-resolved callees named directly by this function body. Missing and
    /// ambiguous origins stay absent here and are diagnosed by body lowering; consumers use this
    /// inventory only to prepare capabilities for declarations which the same lowering reaches.
    pub(crate) fn direct_callees(
        &self,
        identity: &KlibDependencyCallable,
    ) -> Vec<KlibDependencyCallable> {
        let Some((arena, function)) = self.function(identity) else {
            return Vec::new();
        };
        arena
            .call_symbols_in_body(function)
            .filter_map(|symbol| declaration_signature(&symbol.signature))
            .filter_map(
                |declaration| match self.callee_origin(&identity.module, &declaration) {
                    KlibCalleeOrigin::Exact(callee) => Some(callee),
                    KlibCalleeOrigin::Missing | KlibCalleeOrigin::Ambiguous => None,
                },
            )
            .collect()
    }

    /// Whether `identity` is an executable dependency-support root: its own serialized body has a
    /// direct call edge to an intrinsic declaration authenticated from these same selected inputs,
    /// or a direct call edge to one such declaration.
    ///
    /// The single wrapper layer admits a target stdlib's semantic wrapper around its instruction
    /// wrapper, while deliberately not turning every transitive caller into support code. The
    /// eligibility check does not recurse beyond that one edge: an ordinary caller of the semantic
    /// wrapper is not a support root. Producer dependency edges resolve each call before the
    /// intrinsic table is consulted, so an unrelated equal signature cannot grant the capability
    /// and lowering never tries one body before falling back to another realization.
    pub(crate) fn is_supported_intrinsic_root(
        &self,
        identity: &KlibDependencyCallable,
        intrinsics: &super::KlibIntrinsicFacts,
    ) -> bool {
        if self.directly_calls_authenticated_intrinsic(identity, intrinsics) {
            return true;
        }
        let Some((arena, function)) = self.function(identity) else {
            return false;
        };
        arena.call_symbols_in_body(function).any(|symbol| {
            let Some(declaration) = declaration_signature(&symbol.signature) else {
                return false;
            };
            let KlibCalleeOrigin::Exact(callee) =
                self.callee_origin(&identity.module, &declaration)
            else {
                return false;
            };
            self.directly_calls_authenticated_intrinsic(&callee, intrinsics)
        })
    }

    fn directly_calls_authenticated_intrinsic(
        &self,
        identity: &KlibDependencyCallable,
        intrinsics: &super::KlibIntrinsicFacts,
    ) -> bool {
        let Some((arena, function)) = self.function(identity) else {
            return false;
        };
        arena.call_symbols_in_body(function).any(|symbol| {
            let Some(declaration) = declaration_signature(&symbol.signature) else {
                return false;
            };
            let KlibCalleeOrigin::Exact(callee) =
                self.callee_origin(&identity.module, &declaration)
            else {
                return false;
            };
            intrinsics.get(&callee).is_some_and(|callable| {
                matches!(
                    callable.operation(),
                    crate::libraries::CompilerIntrinsic::ReinterpretBits
                        | crate::libraries::CompilerIntrinsic::ScalarInstruction(_)
                )
            })
        })
    }
}

fn find_class(
    arena: &KlibIrArena,
    member: &KlibIrMember,
    classifier: TypeName,
    found: &mut Option<bool>,
) {
    let KlibIrMember::Class(id) = member else {
        return;
    };
    let class = arena.class(*id);
    if class.base.symbol.kind == KlibIrSymbolKind::Class
        && class_identity(&class.base.symbol.signature) == Some(classifier)
    {
        let mut constructors = class.members.iter().filter_map(|member| match member {
            KlibIrMember::Function(function) if arena.function(*function).constructor => {
                Some(arena.function(*function))
            }
            _ => None,
        });
        let inert = class.supertypes.len() == 1
            && inert_any_supertype(arena, class.supertypes[0])
            && constructors.next().is_some_and(|constructor| {
                inert_constructor(arena, constructor, &class.base.symbol)
            })
            && constructors.next().is_none()
            && class.members.iter().all(|member| match member {
                KlibIrMember::AnonymousInitializer { statements, .. } => statements.is_empty(),
                KlibIrMember::Field(field) => field
                    .initializer
                    .is_none_or(|initializer| inert_initializer(arena, initializer)),
                KlibIrMember::Property(property) => {
                    property.backing_field.as_ref().is_none_or(|field| {
                        field
                            .initializer
                            .is_none_or(|initializer| inert_initializer(arena, initializer))
                    })
                }
                KlibIrMember::Function(function) => arena.function(*function).constructor,
                _ => true,
            });
        *found = Some(found.unwrap_or(true) && inert);
    }
    for member in &class.members {
        find_class(arena, member, classifier, found);
    }
}

fn inert_any_supertype(
    arena: &KlibIrArena,
    ty: crate::metadata::klib_ir::tree::KlibIrTypeId,
) -> bool {
    matches!(
        arena.ty(ty),
        KlibIrType::Simple {
            classifier,
            nullability: crate::metadata::klib_ir::tree::KlibIrNullability::DefinitelyNotNull,
            arguments,
            abbreviation: None,
            ..
        } if arguments.is_empty()
            && classifier.kind == KlibIrSymbolKind::Class
            && class_identity(&classifier.signature) == Some(crate::types::wk::any())
    )
}

fn inert_constructor(
    arena: &KlibIrArena,
    constructor: &KlibIrFunction,
    class: &crate::metadata::klib_ir::KlibIrSymbol,
) -> bool {
    if !constructor.type_parameters.is_empty()
        || constructor.dispatch_receiver.is_some()
        || !constructor.context_parameters.is_empty()
        || constructor.extension_receiver.is_some()
        || !constructor.regular_parameters.is_empty()
    {
        return false;
    }
    let Some(KlibIrBody::Block(statements)) = constructor.body.as_ref() else {
        return false;
    };
    let [KlibIrStatement::Expression(delegate), KlibIrStatement::Expression(initialize)] =
        statements.as_slice()
    else {
        return false;
    };
    let KlibIrExprKind::DelegatingConstructorCall(access) = &arena.expr(*delegate).kind else {
        return false;
    };
    if access.symbol.kind != KlibIrSymbolKind::Constructor
        || declaration_owner(&access.symbol.signature) != Some(crate::types::wk::any())
        || !arguments_are_absent(&access.arguments)
    {
        return false;
    }
    matches!(
        &arena.expr(*initialize).kind,
        KlibIrExprKind::InstanceInitializerCall(initialized) if initialized == class
    )
}

fn arguments_are_absent(arguments: &crate::metadata::klib_ir::tree::KlibIrArguments) -> bool {
    match arguments {
        crate::metadata::klib_ir::tree::KlibIrArguments::Flat(arguments) => {
            arguments.iter().all(Option::is_none)
        }
        crate::metadata::klib_ir::tree::KlibIrArguments::Split {
            dispatch_receiver,
            extension_receiver,
            values,
        } => {
            dispatch_receiver.is_none()
                && extension_receiver.is_none()
                && values.iter().all(Option::is_none)
        }
    }
}

fn inert_initializer(
    arena: &KlibIrArena,
    initializer: crate::metadata::klib_ir::tree::KlibIrExprId,
) -> bool {
    matches!(
        arena.expr(initializer).kind,
        crate::metadata::klib_ir::tree::KlibIrExprKind::Const(_)
    )
}

fn class_identity(signature: &KlibIrSignature) -> Option<TypeName> {
    let KlibIrSignature::Public(signature) = signature else {
        return None;
    };
    if signature.member_id().is_some() || signature.declaration().is_empty() {
        return None;
    }
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !signature.package().segments().iter().all(valid)
        || !signature.declaration().segments().iter().all(valid)
    {
        return None;
    }
    let mut identity = signature
        .package()
        .segments()
        .iter()
        .fold(TypeName::ROOT, |parent, segment| {
            type_name_child(parent, segment)
        });
    let mut declarations = signature.declaration().segments().iter();
    identity = type_name_child(identity, declarations.next()?);
    for nested in declarations {
        identity = identity.nested_child(nested);
    }
    Some(identity)
}

fn declaration_owner(signature: &KlibIrSignature) -> Option<TypeName> {
    let KlibIrSignature::Public(signature) = signature else {
        return None;
    };
    let (_, declaration) = signature.declaration().segments().split_last()?;
    if declaration.is_empty() {
        return None;
    }
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !signature.package().segments().iter().all(valid) || !declaration.iter().all(valid) {
        return None;
    }
    let mut identity = signature
        .package()
        .segments()
        .iter()
        .fold(TypeName::ROOT, |parent, segment| {
            type_name_child(parent, segment)
        });
    let mut declarations = declaration.iter();
    identity = type_name_child(identity, declarations.next()?);
    for nested in declarations {
        identity = identity.nested_child(nested);
    }
    Some(identity)
}

/// The compiler-owned semantic discriminator for a function-value invocation. Operand shape alone
/// is insufficient: an `@InlineOnly` wrapper can pass its lambda to an arbitrary helper call.
fn is_direct_lambda_invocation(
    access: &crate::metadata::klib_ir::tree::KlibIrMemberAccess,
) -> bool {
    access.symbol.kind == crate::metadata::klib_ir::KlibIrSymbolKind::Function
        && access.origin.as_deref() == Some("INVOKE")
        && access.type_arguments.iter().all(Option::is_none)
}

fn ir_signature(declaration: &KlibDeclarationSignature) -> KlibIrSignature {
    match declaration {
        KlibDeclarationSignature::Public(signature) => KlibIrSignature::Public(signature.clone()),
        KlibDeclarationSignature::Accessor(signature) => {
            KlibIrSignature::Accessor(signature.clone())
        }
    }
}

fn declaration_signature(signature: &KlibIrSignature) -> Option<KlibDeclarationSignature> {
    match signature {
        KlibIrSignature::Public(signature) => {
            Some(KlibDeclarationSignature::Public(signature.clone()))
        }
        KlibIrSignature::Accessor(signature) => {
            Some(KlibDeclarationSignature::Accessor(signature.clone()))
        }
        KlibIrSignature::FileLocal { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::id_signature::{
        callable_signature, class_signature, CallableShape, ClassScope, DeclarationContainer,
        Placement,
    };
    use crate::metadata::klib_ir::tree::{
        KlibIrArguments, KlibIrDeclarationBase, KlibIrMemberAccess, KlibIrNullability,
        KlibIrParameter, KlibIrTypeParameter,
    };
    use crate::metadata::klib_ir::{KlibIrDeclarationTree, KlibIrSymbol};
    use crate::metadata::semantic::KotlinType;

    fn class_symbol(package: &str, name: &str) -> KlibIrSymbol {
        let package = [package.to_owned()];
        let class = ClassScope::<KotlinType> {
            name,
            type_parameters: Vec::new(),
            expect: false,
        };
        KlibIrSymbol {
            kind: KlibIrSymbolKind::Class,
            signature: KlibIrSignature::Public(class_signature(
                DeclarationContainer {
                    package: &package,
                    classes: &[],
                    native_interop_library: false,
                },
                &class,
            )),
        }
    }

    fn constructor_symbol(package: &str, owner: &str) -> KlibIrSymbol {
        let package = [package.to_owned()];
        let classes = [ClassScope::<KotlinType> {
            name: owner,
            type_parameters: Vec::new(),
            expect: false,
        }];
        let signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &classes,
                native_interop_library: false,
            },
            &CallableShape {
                name: "<init>",
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("an empty constructor is signable");
        KlibIrSymbol {
            kind: KlibIrSymbolKind::Constructor,
            signature: KlibIrSignature::Public(signature),
        }
    }

    fn empty_constructor(
        arena: &mut KlibIrArena,
        class: KlibIrSymbol,
        delegate: KlibIrSymbol,
    ) -> KlibIrFunction {
        let ty = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let delegate = arena.push_expr(
            None,
            KlibIrExprKind::DelegatingConstructorCall(KlibIrMemberAccess {
                symbol: delegate,
                arguments: KlibIrArguments::Flat(Vec::new()),
                type_arguments: Vec::new(),
                origin: None,
            }),
        );
        let initialize = arena.push_expr(None, KlibIrExprKind::InstanceInitializerCall(class));
        KlibIrFunction {
            base: KlibIrDeclarationBase {
                symbol: KlibIrSymbol {
                    kind: KlibIrSymbolKind::Constructor,
                    signature: KlibIrSignature::FileLocal { file: 0, slot: 0 },
                },
                origin: "DEFINED".to_owned(),
                flags: 0,
                annotations: Vec::new(),
            },
            name: "<init>".to_owned(),
            constructor: true,
            type_parameters: Vec::new(),
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver: None,
            regular_parameters: Vec::new(),
            return_type: ty,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: None,
            body: Some(KlibIrBody::Block(vec![
                KlibIrStatement::Expression(delegate),
                KlibIrStatement::Expression(initialize),
            ])),
        }
    }

    fn reified_type_test_bodies(
        operator: KlibIrTypeOperator,
        nullability: KlibIrNullability,
        extension_receiver: bool,
    ) -> (KlibDeclarationBodies, KlibDependencyCallable) {
        let package = ["fixture".to_owned()];
        let signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &[],
                native_interop_library: false,
            },
            &CallableShape::<KotlinType> {
                name: "check",
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("the fixture function is signable");
        let function_symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::Public(signature.clone()),
        };
        let type_parameter_symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::TypeParameter,
            signature: KlibIrSignature::FileLocal { file: 0, slot: 1 },
        };
        let value_symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::ValueParameter,
            signature: KlibIrSignature::FileLocal { file: 0, slot: 2 },
        };
        let mut arena = KlibIrArena::default();
        let error = arena.push_type(KlibIrType::Error {
            annotations: Vec::new(),
        });
        let target = arena.push_type(KlibIrType::Simple {
            classifier: type_parameter_symbol.clone(),
            nullability,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        let read = arena.push_expr(
            Some(error),
            KlibIrExprKind::GetValue {
                symbol: value_symbol.clone(),
                origin: None,
            },
        );
        let test = arena.push_expr(
            Some(error),
            KlibIrExprKind::TypeOperator {
                operator,
                operand: target,
                argument: read,
            },
        );
        let returned = arena.push_expr(
            Some(error),
            KlibIrExprKind::Return {
                target: function_symbol.clone(),
                value: test,
            },
        );
        let base = |symbol| KlibIrDeclarationBase {
            symbol,
            origin: "DEFINED".to_owned(),
            flags: 0,
            annotations: Vec::new(),
        };
        let value_parameter = KlibIrParameter {
            base: base(value_symbol),
            name: "value".to_owned(),
            ty: error,
            vararg_element_type: None,
            default_value: None,
        };
        let (extension_receiver, regular_parameters) = if extension_receiver {
            (Some(value_parameter), Vec::new())
        } else {
            (None, vec![value_parameter])
        };
        let function = arena.push_function(KlibIrFunction {
            base: base(function_symbol),
            name: "check".to_owned(),
            constructor: false,
            type_parameters: vec![KlibIrTypeParameter {
                base: base(type_parameter_symbol),
                name: "T".to_owned(),
                supertypes: Vec::new(),
            }],
            dispatch_receiver: None,
            context_parameters: Vec::new(),
            extension_receiver,
            regular_parameters,
            return_type: error,
            overridden: Vec::new(),
            companion_extension_class: None,
            prepared_inline_file: Some(0),
            body: Some(KlibIrBody::Block(vec![KlibIrStatement::Expression(
                returned,
            )])),
        });
        let trees = KlibIrModuleTrees::from_trees(vec![KlibIrDeclarationTree {
            arena,
            declaration: KlibIrMember::Function(function),
        }])
        .expect("the fixture tree has one public function");
        let identity = KlibDependencyCallable {
            module: "fixture-module".to_owned(),
            declaration: KlibDeclarationSignature::Public(signature),
        };
        let bodies = KlibDeclarationBodies::from_libraries(vec![(identity.module.clone(), trees)])
            .expect("the fixture has one module");
        (bodies, identity)
    }

    #[test]
    fn exact_reified_type_test_body_publishes_symbol_ordinals() {
        let (bodies, identity) = reified_type_test_bodies(
            KlibIrTypeOperator::NotInstanceOf,
            KlibIrNullability::NotSpecified,
            false,
        );
        let Some(crate::libraries::InlineBodyPlan::ReifiedTypeTest {
            value,
            type_parameter,
            negated,
            defaults,
        }) = bodies.reified_type_test_inline_plan(&identity)
        else {
            panic!("the exact body must publish a plan")
        };
        assert_eq!(value, crate::libraries::InlineBodyValue::Parameter(0));
        assert_eq!(type_parameter, 0);
        assert!(negated);
        assert!(defaults.is_empty());
    }

    #[test]
    fn extension_receiver_is_published_as_a_plan_value() {
        let (bodies, identity) = reified_type_test_bodies(
            KlibIrTypeOperator::InstanceOf,
            KlibIrNullability::NotSpecified,
            true,
        );
        let Some(crate::libraries::InlineBodyPlan::ReifiedTypeTest { value, .. }) =
            bodies.reified_type_test_inline_plan(&identity)
        else {
            panic!("the extension body must publish a plan")
        };
        assert_eq!(value, crate::libraries::InlineBodyValue::Parameter(0));
    }

    #[test]
    fn nullable_type_parameter_operand_is_not_the_exact_supported_template() {
        let (bodies, identity) = reified_type_test_bodies(
            KlibIrTypeOperator::InstanceOf,
            KlibIrNullability::MarkedNullable,
            false,
        );
        assert!(bodies.reified_type_test_inline_plan(&identity).is_none());
    }

    #[test]
    fn direct_lambda_invocation_requires_the_compiler_owned_invoke_origin() {
        let symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::Function,
            signature: KlibIrSignature::FileLocal { file: 0, slot: 1 },
        };
        let access = |origin| KlibIrMemberAccess {
            symbol: symbol.clone(),
            arguments: KlibIrArguments::Flat(Vec::new()),
            type_arguments: Vec::new(),
            origin,
        };

        assert!(is_direct_lambda_invocation(&access(Some(
            "INVOKE".to_owned()
        ))));
        assert!(!is_direct_lambda_invocation(&access(None)));
        assert!(!is_direct_lambda_invocation(&access(Some(
            "OTHER".to_owned()
        ))));
    }

    #[test]
    fn body_provider_modules_follow_only_the_producer_manifest_closure() {
        let bodies = KlibDeclarationBodies::from_body_libraries(vec![
            KlibBodyLibrary::new(
                "stdlib".to_owned(),
                ["middle".to_owned()],
                KlibIrModuleTrees::default(),
            ),
            KlibBodyLibrary::new(
                "middle".to_owned(),
                ["leaf".to_owned()],
                KlibIrModuleTrees::default(),
            ),
            KlibBodyLibrary::new(
                "leaf".to_owned(),
                Vec::<String>::new(),
                KlibIrModuleTrees::default(),
            ),
            KlibBodyLibrary::new(
                "unrelated".to_owned(),
                Vec::<String>::new(),
                KlibIrModuleTrees::default(),
            ),
        ])
        .expect("the fixture has unique modules");

        let reachable = bodies.body_provider_modules("stdlib");
        assert_eq!(reachable.len(), 3);
        assert!(reachable.contains("stdlib"));
        assert!(reachable.contains("middle"));
        assert!(reachable.contains("leaf"));
        assert!(!reachable.contains("unrelated"));
    }

    #[test]
    fn inert_singleton_proof_rejects_superclass_and_constructor_effects() {
        let mut arena = KlibIrArena::default();
        let any = class_symbol("kotlin", "Any");
        let side_effect_base = class_symbol("fixture", "SideEffectBase");
        let class = class_symbol("fixture", "PlatformCompanion");
        let any_type = arena.push_type(KlibIrType::Simple {
            classifier: any.clone(),
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        let base_type = arena.push_type(KlibIrType::Simple {
            classifier: side_effect_base,
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        assert!(inert_any_supertype(&arena, any_type));
        assert!(!inert_any_supertype(&arena, base_type));

        let constructor = empty_constructor(
            &mut arena,
            class.clone(),
            constructor_symbol("kotlin", "Any"),
        );
        assert!(inert_constructor(&arena, &constructor, &class));

        let inherited = empty_constructor(
            &mut arena,
            class.clone(),
            constructor_symbol("fixture", "SideEffectBase"),
        );
        assert!(!inert_constructor(&arena, &inherited, &class));

        let mut executable = empty_constructor(
            &mut arena,
            class.clone(),
            constructor_symbol("kotlin", "Any"),
        );
        let effect = arena.push_expr(None, KlibIrExprKind::GetObject(class.clone()));
        let Some(KlibIrBody::Block(statements)) = executable.body.as_mut() else {
            unreachable!()
        };
        statements.push(KlibIrStatement::Expression(effect));
        assert!(!inert_constructor(&arena, &executable, &class));
    }
}
