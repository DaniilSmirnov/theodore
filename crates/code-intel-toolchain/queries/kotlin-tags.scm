; Tags for tree-sitter-grammars/tree-sitter-kotlin v1.1.0: no tags.scm
; upstream (neither does fwcd's), so this is the whole query
; (scripts/build-grammars.sh falls back to this file by language id).
; Node types checked against that grammar's src/node-types.json.
;
; Kotlin has one `class_declaration` for `class`, `interface`, `enum class`
; and the `data`/`sealed`/`fun interface` forms: extract.rs narrows `class`
; to `Interface` by the keyword before the name.

; Definitions
(class_declaration
    name: (identifier) @name) @definition.class

(object_declaration
    name: (identifier) @name) @definition.object

(companion_object
    name: (identifier) @name) @definition.object

(function_declaration
    name: (identifier) @name) @definition.function

(type_alias
    type: (identifier) @name) @definition.type

(enum_entry
    (identifier) @name) @definition.constant

; Properties: top-level and class members only. A `val` inside a function
; body is a local, not a symbol.
(source_file
    (property_declaration
        (variable_declaration (identifier) @name)) @definition.property)

(class_body
    (property_declaration
        (variable_declaration (identifier) @name)) @definition.property)

; `class Money(val amount: Int)`: constructor parameters with val/var are
; properties; plain parameters are not.
(class_parameter
    "val"
    (identifier) @name) @definition.property

(class_parameter
    "var"
    (identifier) @name) @definition.property

; References
(call_expression
    (identifier) @name) @reference.call

(call_expression
    (navigation_expression
        (identifier) @name .)) @reference.call

(delegation_specifier
    (user_type (identifier) @name)) @reference.implementation

(delegation_specifier
    (constructor_invocation
        (user_type (identifier) @name))) @reference.class
