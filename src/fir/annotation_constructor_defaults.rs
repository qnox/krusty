//! Republish annotation-constructor defaults once their checked bodies exist.
//!
//! Pass 1 first records only literals and object singletons. Enum entries, class literals, arrays,
//! and nested annotation instances become closed values here, after every file's default fragment
//! has been checked and before a later file's construction is lowered.

use std::collections::{HashMap, HashSet};

use crate::fir::{
    CallableId, DefaultArgumentStore, FirCallArgument, FirConstant, FirConstructorTarget,
    FirConversion, FirConversionKind, FirExprId, FirExprKind, ResolvedModuleIndex, ResolvedTy,
};
use crate::libraries::DefaultValue;
use crate::types::Ty;

pub(crate) fn publish_checked_annotation_defaults(
    index: &mut ResolvedModuleIndex,
    store: &DefaultArgumentStore,
) {
    let callables = index.annotation_constructor_callables();
    let mut folder = Folder {
        index,
        store,
        memo: HashMap::new(),
        visiting: HashSet::new(),
    };
    let folded = callables
        .iter()
        .copied()
        .map(|callable| (callable, folder.defaults(callable)))
        .collect::<Vec<_>>();
    for (callable, defaults) in folded {
        index.replace_annotation_constructor_defaults(callable, defaults);
    }
}

struct Folder<'a> {
    index: &'a ResolvedModuleIndex,
    store: &'a DefaultArgumentStore,
    memo: HashMap<CallableId, Vec<Option<DefaultValue>>>,
    visiting: HashSet<CallableId>,
}

impl Folder<'_> {
    fn defaults(&mut self, callable: CallableId) -> Vec<Option<DefaultValue>> {
        if let Some(done) = self.memo.get(&callable) {
            return done.clone();
        }
        let published = self
            .index
            .annotation_constructor_defaults(callable)
            .map(<[Option<DefaultValue>]>::to_vec)
            .unwrap_or_default();
        if !self.visiting.insert(callable) {
            return published;
        }
        let mut values = published;
        let slots = self
            .store
            .body(callable)
            .map(|body| {
                body.default_values()
                    .iter()
                    .map(|default| (default.parameter, default.value))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (parameter, value) in slots {
            let Some(folded) = self.expression(callable, value) else {
                continue;
            };
            let Some(slot) = usize::try_from(parameter)
                .ok()
                .and_then(|parameter| values.get_mut(parameter))
            else {
                continue;
            };
            *slot = Some(folded);
        }
        self.visiting.remove(&callable);
        self.memo.insert(callable, values.clone());
        values
    }

    fn expression(&mut self, owner: CallableId, id: FirExprId) -> Option<DefaultValue> {
        let expression = self.store.body(owner)?.expr(id)?.clone();
        match expression.kind {
            FirExprKind::Constant(constant) => Some(constant_value(constant)),
            FirExprKind::EnumEntry {
                classifier, name, ..
            } => Some(DefaultValue::EnumEntry {
                classifier,
                name: name.into_string(),
            }),
            FirExprKind::SingletonValue { classifier } => Some(DefaultValue::Object(classifier)),
            FirExprKind::ClassLiteral {
                classifier: Some(classifier),
                value: None,
            } => class_literal(classifier),
            FirExprKind::ArrayLiteral {
                array_type,
                elements,
            } => self.array(owner, array_type, &elements),
            FirExprKind::AnnotationArray(elements) => {
                self.array_elements(owner, expression.ty, &elements, &[])
            }
            FirExprKind::ImplicitConversion { value, conversion } => {
                if !keeps_closed_value(Some(conversion)) {
                    return None;
                }
                self.expression(owner, value)
            }
            FirExprKind::ConstructorCall(call) => self.annotation_call(owner, expression.ty, &call),
            _ => None,
        }
    }

    fn array(
        &mut self,
        owner: CallableId,
        array_type: ResolvedTy,
        elements: &[crate::fir::FirArrayElement],
    ) -> Option<DefaultValue> {
        if elements.iter().any(|element| element.spread) {
            return None;
        }
        let conversions = elements
            .iter()
            .map(|element| element.conversion)
            .collect::<Vec<_>>();
        let ids = elements
            .iter()
            .map(|element| element.value)
            .collect::<Vec<_>>();
        self.array_elements(owner, array_type, &ids, &conversions)
    }

    fn array_elements(
        &mut self,
        owner: CallableId,
        array_type: ResolvedTy,
        elements: &[FirExprId],
        conversions: &[Option<FirConversion>],
    ) -> Option<DefaultValue> {
        if !conversions.is_empty() && conversions.len() != elements.len() {
            return None;
        }
        let mut folded = Vec::with_capacity(elements.len());
        for (index, element) in elements.iter().copied().enumerate() {
            let conversion = conversions.get(index).copied().flatten();
            if !keeps_closed_value(conversion) {
                return None;
            }
            folded.push(self.expression(owner, element)?);
        }
        Some(DefaultValue::Array {
            array_type: array_type.get(),
            elements: folded,
        })
    }

    fn annotation_call(
        &mut self,
        owner: CallableId,
        ty: ResolvedTy,
        call: &crate::fir::FirConstructorCall,
    ) -> Option<DefaultValue> {
        if call.context_parameter_count != 0
            || call.outer_receiver.is_some()
            || call.external_capture_arguments.is_some()
        {
            return None;
        }
        let FirConstructorTarget::Module {
            declaration,
            annotation: Some(plan),
        } = &call.target
        else {
            return None;
        };
        if plan.members.len() != plan.defaults.len() {
            return None;
        }
        let Ty::Obj(classifier, _) = ty.get() else {
            return None;
        };
        let seeded = self.defaults(*declaration);
        if seeded.len() != plan.members.len() {
            return None;
        }
        let mut values = seeded;
        for argument in call.arguments.iter() {
            match argument {
                FirCallArgument::Default { .. } => {}
                FirCallArgument::Expression {
                    parameter,
                    value,
                    conversion,
                } => {
                    if !keeps_closed_value(*conversion) {
                        return None;
                    }
                    let folded = self.expression(owner, *value)?;
                    let slot = parameter_slot(&mut values, *parameter)?;
                    *slot = Some(folded);
                }
                FirCallArgument::Vararg {
                    parameter,
                    elements,
                    ..
                } => {
                    if elements
                        .iter()
                        .any(|element| element.spread || !keeps_closed_value(element.conversion))
                    {
                        return None;
                    }
                    let array_type = call.parameter_types.get(*parameter as usize)?.get();
                    let mut folded = Vec::with_capacity(elements.len());
                    for element in elements.iter() {
                        folded.push(self.expression(owner, element.value)?);
                    }
                    let slot = parameter_slot(&mut values, *parameter)?;
                    *slot = Some(DefaultValue::Array {
                        array_type,
                        elements: folded,
                    });
                }
            }
        }
        let values = values.into_iter().collect::<Option<Vec<_>>>()?;
        let members = plan
            .members
            .iter()
            .map(|(name, ty)| (name.to_string(), ty.get()))
            .collect();
        Some(DefaultValue::Annotation {
            classifier,
            members,
            values,
        })
    }
}

fn parameter_slot(
    values: &mut [Option<DefaultValue>],
    parameter: u32,
) -> Option<&mut Option<DefaultValue>> {
    values.get_mut(usize::try_from(parameter).ok()?)
}

fn class_literal(classifier: ResolvedTy) -> Option<DefaultValue> {
    let Ty::Obj(name, _) = classifier.get() else {
        return None;
    };
    Some(DefaultValue::KClass(name))
}

fn constant_value(constant: FirConstant) -> DefaultValue {
    match constant {
        FirConstant::Int(value) | FirConstant::UInt(value) => DefaultValue::Int(value),
        FirConstant::Long(value) | FirConstant::ULong(value) => DefaultValue::Long(value),
        FirConstant::Double(value) => DefaultValue::Double(value),
        FirConstant::Float(value) => DefaultValue::Float(value),
        FirConstant::Boolean(value) => DefaultValue::Bool(value),
        FirConstant::String(value) => DefaultValue::Str(value),
        FirConstant::Char(value) => DefaultValue::Char(value),
        FirConstant::Null => DefaultValue::Null,
    }
}

fn keeps_closed_value(conversion: Option<FirConversion>) -> bool {
    match conversion.map(|conversion| conversion.kind) {
        None => true,
        Some(FirConversionKind::NullabilityWidening { .. }) => true,
        Some(_) => false,
    }
}
