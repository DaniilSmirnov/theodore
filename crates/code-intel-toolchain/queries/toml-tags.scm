; Our own tags query for TOML (tree-sitter-toml ships no tags.scm).
;
; Tables (`[table]`, `[[array_table]]`) as module definitions — they are
; TOML's namespaces — and every key = value pair as a field definition.
; The grammar has no `field()` names here (its rules are plain `seq`s), so
; nodes are matched by type; `pair`'s and `table`'s key children are one of
; `bare_key`, `quoted_key` or `dotted_key`.

; [table] / [[array_table]] headers
(table
  (bare_key) @name) @definition.module

(table
  (quoted_key) @name) @definition.module

(table
  (dotted_key) @name) @definition.module

(table_array_element
  (bare_key) @name) @definition.module

(table_array_element
  (quoted_key) @name) @definition.module

(table_array_element
  (dotted_key) @name) @definition.module

; key = value pairs
(pair
  (bare_key) @name) @definition.field

(pair
  (quoted_key) @name) @definition.field

(pair
  (dotted_key) @name) @definition.field
