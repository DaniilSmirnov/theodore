; Our additions to upstream tree-sitter-php's tags.scm (grammars.toml:
; tags_query_local_mode = "append" — concatenated *after* the upstream
; query, not a replacement). Node types checked against tree-sitter-php
; v0.25.1 (php/src/node-types.json).
;
; Upstream covers namespaces, interfaces, traits, classes, properties,
; functions, methods, `new`, and function/static/member calls. It misses:
;   - enums (PHP 8.1): `enum Suit: string { .. }`, and their cases
;   - nullsafe method calls: `$user?->name()`
;   - class constants: `const MAX = 3;`
;   - plain function calls, `helper($x)`: upstream wants the callee to be a
;     qualified name or a variable, but a bare `helper` is a `name`
;   - `new Invoice()` with a bare class name, same reason
;   - `class A extends B`

(enum_declaration
    name: (name) @name) @definition.enum

(enum_case
    name: (name) @name) @definition.constant

(const_declaration
    (const_element (name) @name)) @definition.constant

(nullsafe_member_call_expression
    name: (name) @name) @reference.call

(function_call_expression
    function: (name) @name) @reference.call

(object_creation_expression
    (name) @name) @reference.class

(base_clause
    [(name) (qualified_name)] @name) @reference.class
