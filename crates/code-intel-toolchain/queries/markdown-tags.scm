; Our own tags query for Markdown (tree-sitter-markdown ships no
; tags.scm). markdown_inline (the companion grammar for emphasis, links,
; etc. inside a block) has no definitions or references of its own and is
; skipped — no markdown_inline-tags.scm file.
;
; ATX (`# Heading`) and setext (underlined) headings as module
; definitions, so a doc's own structure shows up in `file_symbols` the way
; a source file's does.

(atx_heading
  heading_content: (inline) @name) @definition.module

(setext_heading
  heading_content: (paragraph) @name) @definition.module
