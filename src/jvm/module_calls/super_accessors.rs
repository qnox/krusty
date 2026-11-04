//! JVM accessor identity and final names for an enclosing instance's selected super call.

use super::ModuleRealizationTarget;
use crate::backend::BackendClassifierSource;
use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile, IrSuperCallKind};
use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Target {
    Function(ResolvedFunctionOverrideTarget),
    Getter(crate::fir::PropertyId),
    Setter(crate::fir::PropertyId),
}

/// The selected operation plus its required JVM ABI. Call-site operand expressions are never
/// part of this identity: each call still evaluates and passes its own receiver and arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AccessorKey {
    target: Target,
    parent: TypeName,
    qualifier: TypeName,
    invocation_owner: TypeName,
    interface: bool,
    direct_receiver: Option<bool>,
    parameters: Vec<Ty>,
    result: Ty,
    descriptor: String,
}

pub(super) struct SelectedAccessor {
    pub(super) key: AccessorKey,
    qualifier_name: String,
}

pub(super) struct AccessorRequest<'a> {
    pub(super) parent: TypeName,
    pub(super) qualifier: TypeName,
    pub(super) invocation_owner: TypeName,
    pub(super) kind: IrSuperCallKind,
    pub(super) declaration: Option<ResolvedFunctionOverrideTarget>,
    pub(super) interface: bool,
    pub(super) direct_receiver: Option<bool>,
    pub(super) parameters: &'a [Ty],
    pub(super) result: Ty,
    pub(super) descriptor: &'a str,
}

struct Accessor {
    selected: SelectedAccessor,
    function: FunId,
    invocation: ExprId,
}

#[derive(Default)]
pub(in crate::jvm) struct SuperAccessors {
    accessors: Vec<Accessor>,
}

impl SuperAccessors {
    pub(super) fn select(
        ir: &IrFile,
        classifiers: &dyn BackendClassifierSource,
        request: AccessorRequest<'_>,
    ) -> Result<SelectedAccessor, ModuleRealizationTarget> {
        let target = match request.kind {
            IrSuperCallKind::Function => Target::Function(
                request
                    .declaration
                    .ok_or(ModuleRealizationTarget::Classifier(request.qualifier))?,
            ),
            IrSuperCallKind::PropertyGetter(property) => Target::Getter(property),
            IrSuperCallKind::PropertySetter(property) => Target::Setter(property),
        };
        Ok(SelectedAccessor {
            key: AccessorKey {
                target,
                parent: request.parent,
                qualifier: request.qualifier,
                invocation_owner: request.invocation_owner,
                interface: request.interface,
                direct_receiver: request.direct_receiver,
                parameters: request.parameters.to_vec(),
                result: request.result,
                descriptor: request.descriptor.to_owned(),
            },
            qualifier_name: qualifier_name(ir, classifiers, request.qualifier)
                .ok_or(ModuleRealizationTarget::Classifier(request.qualifier))?,
        })
    }

    pub(super) fn existing(&self, selected: &SelectedAccessor) -> Option<FunId> {
        self.accessors
            .iter()
            .find(|accessor| accessor.selected.key == selected.key)
            .map(|accessor| accessor.function)
    }

    pub(super) fn record(
        &mut self,
        selected: SelectedAccessor,
        function: FunId,
        invocation: ExprId,
    ) {
        self.accessors.push(Accessor {
            selected,
            function,
            invocation,
        });
    }

    /// Target name passes have now mapped the exact recorded invocation. No semantic source
    /// spelling is used as a recovery name, and no body/descriptor search finds the target again.
    pub(in crate::jvm) fn finalize(&self, ir: &mut IrFile) -> Result<(), ModuleRealizationTarget> {
        let mut names = Vec::with_capacity(self.accessors.len());
        for accessor in &self.accessors {
            let failure = ModuleRealizationTarget::Function(accessor.function);
            let Some(IrExpr::Call { callee, .. }) = ir.exprs.get(accessor.invocation as usize)
            else {
                return Err(failure);
            };
            let name = match &**callee {
                Callee::Special { name, .. } | Callee::Static { name, .. } => name,
                _ => return Err(failure),
            };
            if name.is_empty() {
                return Err(failure);
            }
            if ir.functions.get(accessor.function as usize).is_none() {
                return Err(failure);
            }
            names.push((
                accessor.function,
                accessor_name(name, &accessor.selected.qualifier_name),
            ));
        }
        for (function, name) in names {
            ir.functions[function as usize].name = name;
        }
        Ok(())
    }
}

fn qualifier_name(
    ir: &IrFile,
    classifiers: &dyn BackendClassifierSource,
    qualifier: TypeName,
) -> Option<String> {
    if let Some((ordinal, class)) = ir
        .classes
        .iter()
        .enumerate()
        .find(|(_, class)| class.fq_name_id() == qualifier)
    {
        if let Some(name) = &class.declaration_name {
            return Some(name.name().to_owned());
        }
        // This provenance is the declaration's recorded source segments, not a split physical
        // internal name. Anonymous containers do not supply a named super qualifier.
        return (class.is_local_class && !class.is_anonymous_object)
            .then(|| {
                ir.local_class_name_provenance
                    .get(&(ordinal as crate::ir::ClassId))?
                    .segments
                    .last()
                    .cloned()
            })
            .flatten();
    }
    let name = classifiers.classifier_declaration_name(
        crate::libraries::ClassifierDeclarationKey::Ordinary(qualifier),
    )??;
    Some(name.name().to_owned())
}

fn accessor_name(mapped_function: &str, qualifier_name: &str) -> String {
    // Kotlin 2.4.21 JvmSyntheticAccessorGenerator.syntheticAccessorToSuperSuffix retains the
    // compatibility hash of IrClass.name, not its qualified or JVM internal name.
    let hash = qualifier_name.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(i32::from(unit))
    });
    format!("access${mapped_function}$s{hash}")
}

#[cfg(test)]
mod tests;
