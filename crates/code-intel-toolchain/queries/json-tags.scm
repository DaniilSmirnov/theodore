; Our own tags query for JSON (tree-sitter-json ships no tags.scm).
;
; JSON has no scopes, so there is nothing to call a "reference" — only the
; top-level object's keys are indexed, as field definitions. A key is
; "top-level" when its `pair` is a direct child of the document's own
; `object` (the file's root value); keys inside nested objects are not
; captured, so the symbol list stays short and useful (`find_symbol`,
; `file_symbols`) instead of listing every leaf of a big config file.

(document
  (object
    (pair
      key: (string (string_content) @name)) @definition.field))
