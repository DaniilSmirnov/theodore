; Tags for tree-sitter-c-sharp v0.23.5. This REPLACES upstream's tags.scm
; (grammars.toml: tags_query_local_mode = "replace"), it does not extend it:
; upstream's file ends with `(namespace_declaration ..) @module`, a capture
; tree-sitter-tags rejects ("Invalid capture @module"), so as shipped it does
; not compile and the language would have no syntax layer at all. The first
; part below is upstream's own patterns, minus that last one; the second
; part is what upstream misses. Node types checked against the grammar's
; src/node-types.json.

; ── upstream's ────────────────────────────────────────────────────────────

(class_declaration name: (identifier) @name) @definition.class

(class_declaration (base_list (_) @name)) @reference.class

(interface_declaration name: (identifier) @name) @definition.interface

(interface_declaration (base_list (_) @name)) @reference.interface

(method_declaration name: (identifier) @name) @definition.method

(object_creation_expression type: (identifier) @name) @reference.class

(type_parameter_constraints_clause (identifier) @name) @reference.class

(type_parameter_constraint (type type: (identifier) @name)) @reference.class

(variable_declaration type: (identifier) @name) @reference.class

(invocation_expression function: (member_access_expression name: (identifier) @name)) @reference.send

(namespace_declaration name: (identifier) @name) @definition.module

; ── ours ──────────────────────────────────────────────────────────────────
; Upstream misses:
;   - structs, enums, records (and `record struct`), delegates
;   - properties
;   - local functions
;   - file-scoped namespaces (`namespace Foo;`) and dotted namespace names
;     (`namespace Acme.Shop { .. }`): upstream only matches a one-word name.
;     A dotted name is tagged by its last segment (`Shop`).
;   - calls that are not `x.Method()`: plain `Run()` and `Run<T>()`, and
;     `x.Run<T>()`

(struct_declaration
    name: (identifier) @name) @definition.struct

(enum_declaration
    name: (identifier) @name) @definition.enum

(record_declaration
    name: (identifier) @name) @definition.record

(delegate_declaration
    name: (identifier) @name) @definition.type

(property_declaration
    name: (identifier) @name) @definition.property

(local_function_statement
    name: (identifier) @name) @definition.function

(file_scoped_namespace_declaration
    name: (identifier) @name) @definition.module

(file_scoped_namespace_declaration
    name: (qualified_name
        name: (identifier) @name)) @definition.module

(namespace_declaration
    name: (qualified_name
        name: (identifier) @name)) @definition.module

(invocation_expression
    function: (identifier) @name) @reference.call

(invocation_expression
    function: (generic_name (identifier) @name)) @reference.call

(invocation_expression
    function: (member_access_expression
        name: (generic_name (identifier) @name))) @reference.call
