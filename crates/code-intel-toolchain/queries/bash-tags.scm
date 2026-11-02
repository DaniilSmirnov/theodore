; Our own tags query for Bash (tree-sitter-bash ships no tags.scm).
;
; Function definitions, and every simple command as a reference (by its
; command name) — a script's structure is almost entirely "what calls
; what", so references matter more here than for most languages.

(function_definition
  name: (word) @name) @definition.function

(command
  name: (command_name
    (word) @name)) @reference.call
