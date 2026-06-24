package demo

import com.google.devtools.ksp.*
import com.google.devtools.ksp.processing.*
import com.google.devtools.ksp.symbol.*

annotation class Builder
annotation class Validate

/// A KSP processor exercising a broad slice of the KSP capability matrix (mirroring the categories in
/// google/ksp's kotlin-analysis-api/testData — see `just ksp-corpus`): annotation query, class kind,
/// modifiers/visibility, supertypes, property inspection (type/mutability/const), function + value
/// parameters (incl. vararg), nullability, type resolution (resolve/qualifiedName/nullability/args),
/// getClassDeclarationByName, builtIns, companion, options — plus MULTI-ROUND codegen. Each capability
/// is written into a generated `<Name>Caps` file the krusty-side test asserts on (so the run proves
/// the from-jar processor actually observed the resolved model, not just that it ran).
class BuilderProcessor(private val env: SymbolProcessorEnvironment) : SymbolProcessor {
    override fun process(resolver: Resolver): List<KSAnnotated> {
        for (s in resolver.getSymbolsWithAnnotation("demo.Builder")) {
            if (s is KSClassDeclaration) {
                val pkg = s.packageName.asString()
                val name = s.simpleName.asString()
                val out = env.codeGenerator.createNewFile(Dependencies(false), pkg, "${name}Caps")
                fun line(s: String) = out.write((s + "\n").toByteArray())
                // The capability observations are emitted as COMMENTS so the generated file is valid
                // Kotlin (it must itself compile, step 7). The krusty-side test asserts on these lines.
                fun cap(s: String) = line("// $s")

                line("package $pkg")
                cap("=== KSP capability dump for $name ===")
                cap("classKind=${s.classKind}")
                cap("modifiers=${s.modifiers.map { it.toString() }.sorted()}")
                cap("visibility=${s.getVisibility()}")
                cap("qualifiedName=${s.qualifiedName?.asString()}")
                cap("docString=${s.docString?.trim()}")
                cap("superTypes=${s.superTypes.map { it.resolve().declaration.qualifiedName?.asString() }.toList()}")
                cap("hasCompanion=${s.declarations.filterIsInstance<KSClassDeclaration>().any { it.isCompanionObject }}")
                for (p in s.getAllProperties()) {
                    val t = p.type.resolve()
                    cap("prop ${p.simpleName.asString()} : ${t.declaration.qualifiedName?.asString()} mutable=${p.isMutable} const=${Modifier.CONST in p.modifiers} nullable=${t.nullability}")
                }
                for (f in s.getAllFunctions()) {
                    if (f.simpleName.asString() in setOf("equals", "hashCode", "toString", "<init>")) continue
                    val params = f.parameters.joinToString(",") {
                        "${it.name?.asString()}:${it.type.resolve().declaration.qualifiedName?.asString()}${if (it.isVararg) "(vararg)" else ""}${if (it.hasDefault) "(def)" else ""}"
                    }
                    cap("fun ${f.simpleName.asString()}($params):${f.returnType?.resolve()?.declaration?.qualifiedName?.asString()} kind=${f.functionKind}")
                }
                cap("byName=${resolver.getClassDeclarationByName(s.qualifiedName!!)?.simpleName?.asString()}")
                cap("builtinInt=${resolver.builtIns.intType.declaration.qualifiedName?.asString()}")
                cap("option.greeting=${env.options["greeting"] ?: "<none>"}")
                line("@demo.Validate") // re-trigger a later round
                line("class ${name}Caps")
                out.close()
            }
        }
        // MULTI-ROUND: fires only on the generated @Validate-annotated file.
        for (s in resolver.getSymbolsWithAnnotation("demo.Validate")) {
            if (s is KSClassDeclaration) {
                val pkg = s.packageName.asString()
                val name = s.simpleName.asString()
                val out = env.codeGenerator.createNewFile(Dependencies(false), pkg, "${name}Validator")
                out.write("package $pkg\nclass ${name}Validator\n".toByteArray())
                out.close()
            }
        }
        return emptyList()
    }
}

class BuilderProvider : SymbolProcessorProvider {
    override fun create(environment: SymbolProcessorEnvironment): SymbolProcessor =
        BuilderProcessor(environment)
}
