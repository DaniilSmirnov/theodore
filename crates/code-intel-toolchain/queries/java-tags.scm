; Our additions to upstream tree-sitter-java's tags.scm (grammars.toml:
; tags_query_local_mode = "append" — concatenated *after* the upstream
; query, not a replacement). Node types checked against tree-sitter-java
; v0.23.5 (src/node-types.json).
;
; Upstream covers classes, interfaces, methods, method calls, `new`,
; `extends`, and `implements`. It misses enums, records (Java 16) and
; annotation types (`@interface`), so `find_symbol Color` for an enum
; found nothing.

(enum_declaration
    name: (identifier) @name) @definition.enum

(record_declaration
    name: (identifier) @name) @definition.record

(annotation_type_declaration
    name: (identifier) @name) @definition.interface
