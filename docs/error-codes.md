# Error Codes (core subset)

## Compiler errors
- `unknown_character` a character that isn't part of Check
- `unclosed_string` a string with no closing quote
- `unknown_escape` an unknown escape in a string (`\q`)
- `unclosed_interpolation` a `{` in a string that never closes, or holds no expression
- `leading_zero` a number starting with `0` followed by digits (`017`)
- `malformed_number` any other malformed number (`.5`, `0XFF`, `1E3`, `0b12`)
- `unexpected_token` an unexpected token (the general syntax error)
- `unclosed_bracket` a bracket or brace that never closes
- `trailing_separator` a `,` or `;` at a line end or before `]`, `)` or `}`
- `else_on_new_line` `else` on the line after `}`
- `foreign_operator` an operator from another language (`%`, `&&`, `||`, `!`)
- `assignment_in_condition` `=` in a condition
- `arrow_without_braces` a function with `->` and no braces
- `statement_body_without_braces` a body without braces that is a statement (`let` or an assignment)
- `unknown_name` a name that doesn't exist
- `duplicate_name` a name defined twice in the same block or file
- `redefined_builtin` redefining or shadowing `Maybe`, `Some`, `None`, `Result`, `Ok` or `Err`
- `used_before_declared` a variable or `let` lambda used above its own line
- `used_before_set` a function run before a top-level variable it uses is set
- `nested_named_function` a named function declared inside a function body
- `type_mismatch` a type mismatch (the general one)
- `type_needed` a type that can't be worked out and needs writing
- `wrong_arguments` the wrong number of arguments, or an unknown named one
- `unknown_method` a method that the type doesn't have
- `not_callable` calling something that isn't a function
- `invalid_operand_types` an operator used on types it doesn't work on (`"5" + 3`, `1 and 2`)
- `bitwise_on_bool` a bitwise operator on `Bool`s (suggests `and`, `or`)
- `chained_comparison` chained comparisons (`a < b < c`)
- `slash_for_int` `/` where an `Int` is expected (suggests `div`)
- `branch_type_mismatch` `if` and `else` branches of different types, used as a value
- `return_type_mismatch` return paths of different types
- `recursion_without_return_type` a recursive function without `->`
- `literal_too_big` a number literal too big for its type
- `negative_literal_exponent` a negative literal exponent on an `Int` (`2 ** -1`)
- `unhandled_maybe` a `T?` used where a `T` is needed
- `doubled_maybe` a `T?` body in an `if` without `else` or a value loop, where a plain `T?` is expected
- `flatten_without_nesting` `flatten()` on a `T?` with nothing nested
- `not_iterable` `for` over something that isn't a list or range
- `try_outside_maybe` `try` in a function whose return type can't hold `None` (added by the agent while building, 2026-11-14)
- `assign_to_let` assigning to a `let`
- `assign_to_parameter` assigning to a parameter not written `var`
- `change_let_list` changing a `let` list (`push`, `pop`, `removeAt`)
- `break_outside_loop` `break` or `continue` outside a loop
- `return_outside_function` `return` outside a function
- `loop_value_without_break` a `for`, `while` or `loop` used as a value with no `break` value
- `break_type_mismatch` `break` values of different types in one loop
- `constant_backwards_range` a backwards range with constant ends (`10..0`)
- `nested_type` a class or enum declared inside a function body
- `mixed_fields` a variant with both named and positional fields
- `this_outside_method` `this` outside a method
- `unknown_field` a field that the type doesn't have
- `unqualified_variant` a variant written without its enum where the expected type isn't known (`let s = Circle(1.0)`)
- `non_exhaustive_match` a `match` that misses a case, named in the message
- `or_pattern_bindings` alternatives of an or-pattern that bind different names
- `let_else_can_finish` a `let ... else` whose `else` can finish
- `const_not_constant` a `const` whose value isn't a constant expression
- `const_cycle` consts defined in terms of each other

## Stops
- `try_stopped` a top-level `try` found `None`, so the program stopped (not a panic; exit status 1)

## Panics
- `panic_called` `panic(reason)` was called
- `integer_overflow` integer overflow
- `division_by_zero` integer division by zero (`div`, `mod`, or `/` with two `Int`s)
- `or_panic_on_none` `orPanic` on a `None`
- `negative_exponent` a negative exponent on an `Int`
- `shift_out_of_range` a shift amount below 0 or of 64 and above
- `round_out_of_range` rounding `NaN`, `Infinity` or a `Float` too big for an `Int`
- `stack_overflow` more than 10,000 nested calls
- `index_out_of_range` writing to a list index that doesn't exist (`list[9] = x`)

## Warnings
- `unused_var` a `var` that's never reassigned
- `shadowed_name` a local that shadows a function or type
- `interpolated_maybe` interpolating a `T?`
- `nested_maybe` an `if` or loop value that is a `T??`
- `let_else_some` `let Some(x) = e else { ... }`, which `let x = e ?? ...` replaces
