package javaarrays;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.RUNTIME)
public @interface Valued {
    String[] value();
    long[] more() default {};
}
