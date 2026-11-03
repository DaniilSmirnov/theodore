; Our own tags query for YAML (tree-sitter-yaml ships no tags.scm).
;
; Block-style mapping keys, two levels deep, as field definitions:
;
;   level: 1            # captured
;     nested: 2          # captured (level 2, value of a level-1 key)
;       deeper: 3         # not captured — a third level would make the
;                         # symbol list as noisy as the file itself.
;
; Flow mappings (`{a: 1}`) are not covered — rare in hand-written YAML,
; and config files (the main use here) are written in block style.

; level 1: the document's own top-level mapping
(document
  (block_node
    (block_mapping
      (block_mapping_pair
        key: (_) @name) @definition.field)))

; level 2: keys of a mapping that is the value of a level-1 key
(document
  (block_node
    (block_mapping
      (block_mapping_pair
        value: (block_node
          (block_mapping
            (block_mapping_pair
              key: (_) @name) @definition.field))))))
