; Roadbook highlights.
;
; Every node is captured by at most one pattern, so the result does not depend on whether an
; editor lets the first or the last matching pattern win. Upper-case names are types, errors,
; events, states and machines; lower-case names are variables, fields, functions, actions and
; tasks. Which of them a name is follows from where it stands.

; ─── Comments ───

(comment) @comment

(doc_comment) @comment.documentation

; ─── Keywords ───

[
  "type"
  "enum"
  "error"
  "extern"
  "machine"
  "var"
  "event"
  "emits"
  "children"
  "by"
  "state"
  "history"
  "initial"
  "entry"
  "exit"
  "on"
  "after"
  "always"
  "done"
  "invoke"
  "as"
  "retry"
  "output"
  "fail"
  "set"
  "do"
  "send"
  "emit"
  "spawn"
  "stop"
  "ref"
] @keyword

[
  "fn"
  "action"
  "task"
] @keyword.function

[
  "import"
  "from"
] @keyword.import

[
  "final"
  "parallel"
  "deep"
] @keyword.modifier

(sensitive) @keyword.modifier

[
  "if"
  "else"
] @keyword.conditional

[
  "and"
  "or"
  "not"
] @keyword.operator

; ─── Types ───

(builtin_type) @type.builtin

(map_type
  "map" @type.builtin)

(type_parameters
  parameter: (type_identifier) @type.parameter)

(generic_type
  name: (type_identifier) @type)

(type_arguments
  argument: (type_identifier) @type)

(map_type
  key: (type_identifier) @type)

(map_type
  value: (type_identifier) @type)

(type_declaration
  name: (type_identifier) @type.definition)

(enum_declaration
  name: (type_identifier) @type.definition)

(error_declaration
  name: (type_identifier) @type.definition)

(extern_declaration
  name: (type_identifier) @type.definition)

(machine_declaration
  name: (type_identifier) @type.definition)

(type_declaration
  type: (type_identifier) @type)

(field_declaration
  type: (type_identifier) @type)

(parameter
  type: (type_identifier) @type)

(variable_declaration
  type: (type_identifier) @type)

(list_type
  element: (type_identifier) @type)

(optional_type
  type: (type_identifier) @type)

(ref_type
  machine: (type_identifier) @type)

(function_declaration
  return_type: (type_identifier) @type)

(task_declaration
  return_type: (type_identifier) @type)

(machine_declaration
  return_type: (type_identifier) @type)

(errors
  error: (type_identifier) @type)

(fail_transition
  error: (type_identifier) @type)

(child_fail
  error: (type_identifier) @type)

(import
  name: (type_identifier) @type)

; A machine run by `invoke`.
(invoke
  callee: (type_identifier) @type)

; A collection's machine and key type, and a machine started by `spawn`.
(children_declaration
  machine: (type_identifier) @type)

(children_declaration
  key: (type_identifier) @type)

(spawn
  machine: (type_identifier) @type)

; ─── States and events ───

(state_declaration
  name: (type_identifier) @label)

(history_state
  name: (type_identifier) @label)

(initial
  state: (type_identifier) @label)

(target
  state: (type_identifier) @label)

(event_declaration
  name: (type_identifier) @constant)

(emits_declaration
  name: (type_identifier) @constant)

(event_pattern
  event: (type_identifier) @constant)

(received_event
  event: (type_identifier) @constant)

(send
  event: (type_identifier) @constant)

(emit
  event: (type_identifier) @constant)

(wildcard) @character.special

; ─── Values ───

(enum_declaration
  variant: (type_identifier) @constant)

(enum_value
  type: (type_identifier) @type
  variant: (type_identifier) @constant)

(record_value
  type: (type_identifier) @constructor)

(implicit_name) @variable.builtin

(boolean) @boolean

(integer) @number

(float) @number.float

(duration) @number

(string) @string

(escape_sequence) @string.escape

; ─── Functions, actions and tasks ───

(function_declaration
  name: (identifier) @function)

(action_declaration
  name: (identifier) @function)

(task_declaration
  name: (identifier) @function)

(call
  function: (identifier) @function.call)

(invoke
  callee: (identifier) @function.call)

(import
  name: (identifier) @function)

; ─── Variables, parameters and fields ───

(parameter
  name: (identifier) @variable.parameter)

(field_declaration
  name: (identifier) @property)

(field_value
  name: (identifier) @property)

(field_expression
  field: (identifier) @property)

(variable_declaration
  name: (identifier) @variable)

(assignment
  variable: (identifier) @variable)

(invoke
  alias: (identifier) @variable)

(received_event
  from: (identifier) @variable)

(send
  to: (identifier) @variable)

(children_declaration
  name: (identifier) @variable)

(spawn
  collection: (identifier) @variable)

(stop
  collection: (identifier) @variable)

(child_done
  from: (identifier) @variable)

(child_fail
  from: (identifier) @variable)

; A name read in an expression.
(_
  value: (identifier) @variable)

(_
  left: (identifier) @variable)

(_
  right: (identifier) @variable)

(_
  operand: (identifier) @variable)

(_
  condition: (identifier) @variable)

(_
  delay: (identifier) @variable)

(_
  key: (identifier) @variable)

(arguments
  (identifier) @variable)

(parenthesized_expression
  (identifier) @variable)

; ─── Operators and punctuation ───

[
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "+"
  "-"
  "*"
  "="
  "->"
  "!"
  "|"
  "?"
] @operator

[
  "{"
  "}"
  "("
  ")"
  "["
  "]"
] @punctuation.bracket

[
  ","
  "."
  ":"
] @punctuation.delimiter
