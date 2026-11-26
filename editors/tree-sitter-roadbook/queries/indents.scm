; Neovim-style indents: two spaces inside every bracketed block or list.

[
  (import)
  (enum_declaration)
  (fields)
  (parameters)
  (arguments)
  (machine_body)
  (state_body)
  (invoke)
  (branches)
  (record_body)
] @indent.begin

[
  "}"
  ")"
] @indent.branch @indent.end

[
  (comment)
  (doc_comment)
] @indent.auto
