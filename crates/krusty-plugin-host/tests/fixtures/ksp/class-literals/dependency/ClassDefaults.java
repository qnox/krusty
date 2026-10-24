package javaclasses;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.RUNTIME)
public @interface ClassDefaults {
    Class<?> none() default void.class;
    Class<?> primitive() default int.class;
    Class<?> boxed() default Integer.class;
    Class<?> text() default String.class;
    Class<?> list() default java.util.List.class;
    Class<?> array() default int[].class;
    Class<?> objects() default String[].class;
    Class<? extends Number> bounded() default Long.class;
}
