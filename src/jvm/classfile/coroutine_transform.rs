//! kotlinc's coroutine transformation of a named suspend function, run when its class is written.
//!
//! kotlinc's codegen emits a suspend function's body with each suspension point left between
//! `InlineMarker` calls, inlines into it, and only then hands the finished method to
//! `CoroutineTransformerMethodVisitor`, which builds the state machine over it (see
//! `docs/JVM_INLINE_BEFORE_CPS.md`). krusty does the same: the emitter asks for the transformation
//! when it adds the method, and it runs here, over the finished body with every table attached,
//! before the rest of kotlinc's bytecode rewrites. What the state machine needs from its
//! continuation class (the spill fields and `@DebugMetadata`) is handed back to the emitter, which
//! writes that class afterwards.

use super::constant_pool_queries::PoolLookup;
use super::method_rewrite::MethodIdentity;
use super::ClassWriter;
use crate::jvm::bytecode_passes::coroutines::{
    transform_named_function, CoroutineError, DebugMetadata, NamedFunction, SpillField, Transformed,
};

/// A method the emitter asks the transformer to rewrite, and what the transformer needs about it
/// beyond its body (the rest of [`NamedFunction`] is the class's own).
#[derive(Clone, Debug)]
pub(crate) struct CoroutineRequest {
    /// The internal name of the function's continuation class.
    pub continuation_class: String,
    /// The function's first line.
    pub line_number: u16,
    /// The physical slot of the `$completion` parameter, as the emitter assigned it.
    pub completion_slot: u16,
}

/// What the transformation of one function found.
#[derive(Clone, Debug)]
pub(crate) enum CoroutineOutcome {
    /// A state machine: its continuation class declares these spill fields, in this order, and
    /// carries this `@DebugMetadata`.
    StateMachine {
        fields: Vec<SpillField>,
        debug_metadata: DebugMetadata,
    },
    /// Every suspension point is a tail call: no continuation class is written.
    TailCalls,
    /// The body could not be transformed; the class cannot be written correctly.
    Failed(String),
}

/// One transformed function, by its continuation class.
#[derive(Clone, Debug)]
pub(crate) struct TransformedCoroutine {
    pub continuation_class: String,
    pub outcome: CoroutineOutcome,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Coroutines {
    /// `(name index, descriptor index, request)` of each method to transform.
    requests: Vec<(u16, u16, CoroutineRequest)>,
}

impl ClassWriter {
    /// Ask for the method `name``desc`, already added, to be transformed into a state machine when
    /// the class is written.
    pub(crate) fn request_coroutine_transform(
        &mut self,
        name: &str,
        desc: &str,
        request: CoroutineRequest,
    ) {
        let (name, desc) = (self.cp.utf8(name), self.cp.utf8(desc));
        self.coroutines.requests.push((name, desc, request));
    }

    /// Transform every requested method in place.
    pub(super) fn transform_coroutines(&mut self) -> Vec<TransformedCoroutine> {
        std::mem::take(&mut self.coroutines.requests)
            .into_iter()
            .map(|(name, desc, request)| {
                let outcome = self
                    .transform_coroutine(name, desc, &request)
                    .unwrap_or_else(CoroutineOutcome::Failed);
                crate::trace_compiler!(
                    "suspend",
                    "transformed {} -> {outcome:?}",
                    request.continuation_class
                );
                TransformedCoroutine {
                    continuation_class: request.continuation_class,
                    outcome,
                }
            })
            .collect()
    }

    fn transform_coroutine(
        &mut self,
        name: u16,
        desc: u16,
        request: &CoroutineRequest,
    ) -> Result<CoroutineOutcome, String> {
        let index = self
            .methods
            .iter()
            .position(|method| method.name == name && method.desc == desc)
            .ok_or("the method to transform was never added")?;
        let method = &self.methods[index];
        let bytes = method.code.as_deref().ok_or("the method has no body")?;
        let source = method
            .rewrite_source
            .as_deref()
            .ok_or("the method has no rewrite source")?;
        let (access, method_name, method_desc) =
            (source.access, source.name.clone(), source.desc.clone());
        let pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let node = self
            .finished_node(method, source, bytes, &pool)
            .ok_or("the finished body cannot be read")?
            .node;
        let source_file = self
            .source_file
            .clone()
            .ok_or("the transformed source method has no SourceFile identity")?;
        let owner = self.internal_name.clone();
        let function = NamedFunction {
            owner: &owner,
            continuation_class: &request.continuation_class,
            source_file: &source_file,
            line_number: request.line_number,
            completion_slot: request.completion_slot,
            dispatch_receiver: None,
        };
        let (node, outcome) =
            match transform_named_function(node, &function).map_err(|error| describe(&error))? {
                Transformed::TailCalls(node) => (node, CoroutineOutcome::TailCalls),
                Transformed::StateMachine(machine) => (
                    machine.method,
                    CoroutineOutcome::StateMachine {
                        fields: machine.layout.fields,
                        debug_metadata: machine.debug_metadata,
                    },
                ),
            };
        // The transformed body's constants intern here, in its instruction order, as kotlinc's
        // writer interns them when the transformed method is visited.
        let assembled = node
            .assemble(self)
            .map_err(|error| format!("the transformed body does not assemble: {error:?}"))?;
        let lvt = assembled
            .local_variables
            .iter()
            .map(|local| {
                (
                    self.cp.utf8(&local.name),
                    self.cp.utf8(&local.desc),
                    local.slot,
                    Some(local.start_pc),
                    Some(local.length),
                )
            })
            .collect();
        let method = &mut self.methods[index];
        method.max_stack = assembled.max_stack;
        method.max_locals = assembled.max_locals;
        method.code = Some(assembled.code);
        method.exceptions = assembled.exception_table;
        method.lnt = assembled.line_numbers;
        method.lvt = lvt;
        method.implicit_void_return_pc = None;
        // The rewrites for emitted bodies read the emitter's labels, which the transformed body no
        // longer lays out as.
        method.rewrite_source = None;
        // kotlinc's optimizer takes the transformer's method, as its visitor chain hands it on.
        let identity = MethodIdentity {
            access,
            name: &method_name,
            desc: &method_desc,
        };
        let mut pool = PoolLookup::new(&self.cp, &self.bootstrap_methods);
        let optimized = self.optimized(&self.methods[index], identity, node, None, &mut pool);
        if let Some(optimized) = optimized {
            self.methods[index].take_rewritten(optimized);
        }
        Ok(outcome)
    }
}

fn describe(error: &CoroutineError) -> String {
    format!("the coroutine transformation failed: {error:?}")
}
