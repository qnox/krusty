package javaarrays;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;

@Retention(RetentionPolicy.RUNTIME)
public @interface ArrayElements {
    String[] names();
    int count();
    Class<?>[] types();
    String[] tags() default {"x"};
    int[] numbers();
}
