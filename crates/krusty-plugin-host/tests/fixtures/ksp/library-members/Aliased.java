package dependency;

import java.lang.annotation.Annotation;
import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.RUNTIME)
public @interface Aliased {
    @Alias(annotation = Retention.class, member = "value")
    String value() default "/";

    @Alias(annotation = Target.class)
    @Note
    String[] produces() default {"application/json"};

    String port() default "";

    String plain();

    @Retention(RetentionPolicy.RUNTIME)
    @Target({ElementType.PARAMETER, ElementType.TYPE, ElementType.METHOD, ElementType.FIELD})
    @interface Alias {
        Class<? extends Annotation> annotation();

        String member() default "value";
    }

    @Retention(RetentionPolicy.CLASS)
    @interface Note {}
}
