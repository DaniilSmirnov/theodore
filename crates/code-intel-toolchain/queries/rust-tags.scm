; Our additions to upstream tree-sitter-rust's tags.scm (grammars.toml:
; tags_query_local_mode = "append" — this file is concatenated *after* the
; upstream query, not a replacement for it; see that file's comment for
; why).
;
; Two gaps in upstream's query (node-types.json for `impl_item`,
; `call_expression`, confirmed against tree-sitter-rust v0.24.2):
;
; 1. Scoped calls. Upstream only matches a bare `(identifier)` as a call's
;    function, so `Foo::new()` — function is a `scoped_identifier`, not an
;    `identifier` — is invisible to `references`/`find_symbol`.
;
; 2. Generic / scoped impl containers. `impl_item`'s `trait` and `type`
;    fields can be a `type_identifier` (what upstream handles), a
;    `scoped_type_identifier` (`impl fmt::Display for X`) or a
;    `generic_type` (`impl Foo<T> for X`, `impl Foo<T> { .. }`). Upstream's
;    `@reference.implementation` patterns only match the plain
;    `type_identifier` case, so those two shapes never become a tag at
;    all — `extract.rs`'s `impl_target` (reads the header text itself,
;    after `for`) never runs because there is no tag to run it on, and the
;    impl's methods are left at kind `function` instead of `method` with
;    no container.
;
; `@reference.implementation` tags for a trait impl (header has `for`) get
; their container label from `extract.rs::impl_target`, which re-reads the
; header text itself — the @name capture's value does not matter there.
; For an inherent impl (no `for`) there is no such override, so @name
; *is* the label: captured as the generic/scoped type's own base name
; (`Foo`, not `Foo<T>`) to match the plain `type_identifier` case upstream
; already handles.

; 1. scoped calls: `Foo::new()`, `std::mem::swap(..)`
(call_expression
    function: (scoped_identifier
        name: (identifier) @name)) @reference.call

; 2. impl of a scoped trait: `impl fmt::Display for X`
(impl_item
    trait: (scoped_type_identifier
        name: (type_identifier) @name)) @reference.implementation

; 2. impl of a generic trait: `impl Foo<T> for X`
(impl_item
    trait: (generic_type
        type: (_) @name)) @reference.implementation

; 2. inherent impl of a generic type: `impl Foo<T> { .. }`
(impl_item
    type: (generic_type
        type: (_) @name)
    !trait) @reference.implementation

; 2. inherent impl of a scoped type: `impl path::Foo { .. }`
(impl_item
    type: (scoped_type_identifier
        name: (type_identifier) @name)
    !trait) @reference.implementation
