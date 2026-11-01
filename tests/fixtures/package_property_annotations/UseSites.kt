package sites

@Target(AnnotationTarget.PROPERTY_GETTER)
annotation class GetterMark

@Target(AnnotationTarget.PROPERTY_SETTER)
annotation class SetterMark

@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class ParameterMark

@Target(AnnotationTarget.FIELD)
annotation class FieldMark

@Target(AnnotationTarget.PROPERTY)
annotation class PropertyMark

@Retention(AnnotationRetention.SOURCE)
@Target(AnnotationTarget.PROPERTY, AnnotationTarget.FIELD, AnnotationTarget.PROPERTY_GETTER)
annotation class SourceMark

@get:GetterMark val getterOnly = 1
@set:SetterMark var setterOnly = 2
@setparam:ParameterMark var setterParameterOnly = 3
@field:FieldMark val fieldOnly = 4
@property:PropertyMark val propertyOnly = 5
@field:FieldMark @get:GetterMark var fieldAndGetter = 6
@property:PropertyMark @get:GetterMark var propertyAndGetter = 7
@get:SourceMark val sourceGetter = 8
@field:SourceMark val sourceField = 9
@property:SourceMark val sourceProperty = 10
