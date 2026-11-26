; Roadbook locals: a machine's parameters and variables, and the names `invoke … as` gives
; the machines it runs, which are in scope in the invoking state and the states inside it.

(machine_declaration) @local.scope

(state_declaration) @local.scope

(parameter
  name: (identifier) @local.definition)

(variable_declaration
  name: (identifier) @local.definition)

(invoke
  alias: (identifier) @local.definition)

; Names read in expressions, assigned, or used as the address of a message.
(_
  value: (identifier) @local.reference)

(_
  left: (identifier) @local.reference)

(_
  right: (identifier) @local.reference)

(_
  operand: (identifier) @local.reference)

(_
  condition: (identifier) @local.reference)

(_
  delay: (identifier) @local.reference)

(arguments
  (identifier) @local.reference)

(parenthesized_expression
  (identifier) @local.reference)

(assignment
  variable: (identifier) @local.reference)

(send
  to: (identifier) @local.reference)

(received_event
  from: (identifier) @local.reference)
