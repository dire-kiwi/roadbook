/**
 * @file Roadbook: a statechart language (`.road` files).
 * @license MIT OR Apache-2.0
 *
 * Follows the grammar in docs/design/language.md ("Grammar"), production by production.
 * Names starting with an upper-case letter are `type_identifier`s (types, errors, events,
 * states, machines, enum variants); lower-case ones are `identifier`s (variables, fields,
 * functions, actions, tasks). Fields say which role a name plays, for queries.
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const commaSep1 = (rule) => seq(rule, repeat(seq(',', rule)), optional(','));
const commaSep = (rule) => optional(commaSep1(rule));

module.exports = grammar({
  name: 'roadbook',

  extras: ($) => [/\s/, $.comment, $.doc_comment],

  word: ($) => $.identifier,

  // Every keyword is reserved: none can be a name. `from`, `as`, `retry`, `ref`, `sensitive`,
  // `history`, `deep`, `children`, `by`, `spawn`, `stop`, `map`, the words of a check (`check`,
  // `never`, `reachable`, `only`, `can`, `finish`) and the built-in type names are not, because
  // they mean something only where no name can appear.
  reserved: {
    global: (_) => [
      'import', 'type', 'enum', 'error', 'extern', 'fn', 'action', 'task', 'machine', 'strict',
      'var', 'event', 'emits', 'initial', 'state', 'final', 'parallel', 'entry', 'exit', 'on',
      'after', 'always', 'done', 'invoke', 'output', 'fail', 'if', 'else', 'set', 'do', 'send',
      'emit', 'ignore', 'and', 'or', 'not', 'true', 'false', 'call', 'reply',
    ],
  },

  conflicts: ($) => [
    // `Role.Admin` (an enum value) against `Thing.field` (a field read of a record value):
    // told apart by the case of the name after the `.`, one token further on.
    [$.record_value, $.enum_value],
    // `reply pending done` and what follows it: in a machine body, a `call` or `event`
    // declaration starts with a keyword that is also an implicit name, so the parser reads on
    // to the name after it, which no value can be followed by.
    [$.reply],
  ],

  rules: {
    source_file: ($) => repeat(choice($.import, $._declaration)),

    // ───────────── imports and declarations ─────────────

    import: ($) =>
      seq(
        'import',
        '{',
        commaSep1(field('name', $._name)),
        '}',
        'from',
        field('path', $.string),
      ),

    _declaration: ($) =>
      choice(
        $.type_declaration,
        $.enum_declaration,
        $.error_declaration,
        $.extern_declaration,
        $.function_declaration,
        $.action_declaration,
        $.task_declaration,
        $.machine_declaration,
      ),

    extern_declaration: ($) => seq('extern', 'type', field('name', $.type_identifier)),

    type_declaration: ($) =>
      seq(
        'type',
        field('name', $.type_identifier),
        optional(field('parameters', $.type_parameters)),
        choice(seq('=', field('type', $._type)), field('fields', $.fields)),
      ),

    enum_declaration: ($) =>
      seq(
        'enum',
        field('name', $.type_identifier),
        '{',
        commaSep1(field('variant', $.type_identifier)),
        '}',
      ),

    error_declaration: ($) =>
      seq('error', field('name', $.type_identifier), optional(field('fields', $.fields))),

    fields: ($) => seq('{', commaSep($.field_declaration), '}'),

    // `sensitive` is not reserved: a modifier before a field or parameter name, or `var`, and a
    // name anywhere else, a field's or parameter's too (`sensitive: bool`).
    field_declaration: ($) =>
      seq(optional($.sensitive), field('name', $._member_name), ':', field('type', $._type)),

    _member_name: ($) => choice($.identifier, alias('sensitive', $.identifier)),

    sensitive: (_) => 'sensitive',

    function_declaration: ($) =>
      seq(
        'fn',
        field('name', $.identifier),
        field('parameters', $.parameters),
        '->',
        field('return_type', $._type),
      ),

    action_declaration: ($) =>
      seq('action', field('name', $.identifier), field('parameters', $.parameters)),

    task_declaration: ($) =>
      seq(
        'task',
        field('name', $.identifier),
        field('parameters', $.parameters),
        optional(seq('->', field('return_type', $._type))),
        optional(field('errors', $.errors)),
      ),

    parameters: ($) => seq('(', commaSep($.parameter), ')'),

    parameter: ($) =>
      seq(optional($.sensitive), field('name', $._member_name), ':', field('type', $._type)),

    errors: ($) =>
      seq('!', field('error', $.type_identifier), repeat(seq('|', field('error', $.type_identifier)))),

    // ───────────── types ─────────────

    // `type Page<T, U>`
    type_parameters: ($) => seq('<', commaSep1(field('parameter', $.type_identifier)), '>'),

    _type: ($) =>
      choice(
        $.type_identifier,
        $.generic_type,
        $.builtin_type,
        $.ref_type,
        $.call_type,
        $.map_type,
        $.list_type,
        $.optional_type,
      ),

    builtin_type: (_) => choice('string', 'int', 'float', 'bool', 'duration', 'json'),

    ref_type: ($) => seq('ref', field('machine', $.type_identifier)),

    // A pending call, kept until it is replied to: `var pending: call reserve`.
    call_type: ($) => seq('call', field('call', $.identifier)),
    // `Page<Order>`
    generic_type: ($) => seq(field('name', $.type_identifier), field('arguments', $.type_arguments)),

    type_arguments: ($) => seq('<', commaSep1(field('argument', $._type)), '>'),

    // `map<string, Order>`
    map_type: ($) =>
      seq('map', '<', field('key', $._type), ',', field('value', $._type), optional(','), '>'),

    list_type: ($) => seq(field('element', $._type), '[', ']'),

    optional_type: ($) => seq(field('type', $._type), '?'),

    // ───────────── machines ─────────────

    machine_declaration: ($) =>
      seq(
        optional(field('modifier', 'strict')),
        'machine',
        field('name', $.type_identifier),
        field('parameters', $.parameters),
        optional(seq('->', field('return_type', $._type))),
        optional(field('errors', $.errors)),
        field('body', $.machine_body),
      ),

    machine_body: ($) => seq('{', repeat($._machine_item), '}'),

    _machine_item: ($) =>
      choice(
        $.variable_declaration,
        $.event_declaration,
        $.emits_declaration,
        $.call_declaration,
        $.children_declaration,
        $.check,
        $._common_item,
      ),

    variable_declaration: ($) =>
      seq(
        optional($.sensitive),
        'var',
        field('name', $.identifier),
        ':',
        field('type', $._type),
        optional(seq('=', field('value', $._expression))),
      ),

    event_declaration: ($) =>
      seq('event', field('name', $.type_identifier), optional(field('fields', $.fields))),

    emits_declaration: ($) =>
      seq('emits', field('name', $.type_identifier), optional(field('fields', $.fields))),

    // A call the machine answers: `call reserve(orderId: string) -> Reservation ! OutOfStock`.
    call_declaration: ($) =>
      seq(
        'call',
        field('name', $.identifier),
        field('parameters', $.parameters),
        optional(seq('->', field('return_type', $._type))),
        optional(field('errors', $.errors)),
      ),

    // `children sessions: Session by SessionId`
    children_declaration: ($) =>
      seq(
        'children',
        field('name', $.identifier),
        ':',
        field('machine', $.type_identifier),
        'by',
        field('key', $._type),
      ),

    // `check never A and B`, `check reachable A`, `check A only after B`, `check can finish`:
    // only directly in a machine body. A state path is written like a target.
    check: ($) =>
      seq(
        'check',
        field(
          'property',
          choice($.never_property, $.reachable_property, $.only_after_property, $.can_finish_property),
        ),
      ),

    never_property: ($) =>
      seq('never', field('state', $.state_path), repeat(seq('and', field('state', $.state_path)))),

    reachable_property: ($) => seq('reachable', field('state', $.state_path)),

    only_after_property: ($) =>
      seq(field('state', $.state_path), 'only', 'after', field('after', $.state_path)),

    can_finish_property: (_) => seq('can', 'finish'),

    state_path: ($) => seq(field('state', $.type_identifier), repeat(seq('.', field('state', $.type_identifier)))),

    // ───────────── states ─────────────

    state_declaration: ($) =>
      seq(
        optional(field('kind', choice('final', 'parallel'))),
        'state',
        field('name', $.type_identifier),
        field('body', $.state_body),
      ),

    state_body: ($) => seq('{', repeat($._state_item), '}'),

    _state_item: ($) =>
      choice($._common_item, $.history_state, $.done_transition, $.invoke, $.output, $.fail),

    // `history` and `deep` are not reserved: they mean something only at the start of an item.
    history_state: ($) =>
      seq(optional(field('kind', 'deep')), 'history', field('name', $.type_identifier)),

    _common_item: ($) =>
      choice(
        $.initial,
        $.state_declaration,
        $.entry,
        $.exit,
        $.on_transition,
        $.after_transition,
        $.always_transition,
      ),

    initial: ($) => seq('initial', field('state', $.type_identifier)),

    entry: ($) => seq('entry', $._effects),

    exit: ($) => seq('exit', $._effects),

    on_transition: ($) => seq('on', field('trigger', $._trigger), $._branches),

    _trigger: ($) =>
      choice($.event_pattern, $._wildcard, $.received_event, $.call_pattern, $.child_done, $.child_fail),

    event_pattern: ($) => field('event', $.type_identifier),

    // `on reserve`: taking a call the machine declares. Lower case, and no `.` after it.
    call_pattern: ($) => field('call', $.identifier),

    _wildcard: ($) => alias('*', $.wildcard),

    received_event: ($) =>
      seq(field('from', $.identifier), '.', field('event', $.type_identifier)),

    // `on sessions.done` and `on sessions.fail Lost`: a spawned child of a collection ended.
    child_done: ($) => seq(field('from', $.identifier), '.', 'done'),

    child_fail: ($) =>
      seq(
        field('from', $.identifier),
        '.',
        'fail',
        field('error', choice($.type_identifier, $._wildcard)),
      ),

    after_transition: ($) => seq('after', field('delay', $._expression), $._branches),

    always_transition: ($) => seq('always', $._branches),

    done_transition: ($) => seq('done', $._branches),

    invoke: ($) =>
      seq(
        'invoke',
        choice(
          field('callee', $._name),
          // `invoke inventory.reserve(…)`: a call to the peer a `ref` names.
          seq(field('peer', $.identifier), '.', field('callee', $.identifier)),
        ),
        field('arguments', $.arguments),
        optional(seq('as', field('alias', $.identifier))),
        optional(field('retry', $.retry)),
        '{',
        repeat1(choice($.done_transition, $.fail_transition)),
        '}',
      ),

    // `retry 3 on Busy | Gone after backoff(attempt)`: a record value as the delay must be
    // parenthesised, since the invoke's `{` follows, as after an `after` trigger.
    retry: ($) =>
      seq(
        'retry',
        field('count', $.integer),
        optional(
          seq('on', field('error', $.type_identifier), repeat(seq('|', field('error', $.type_identifier)))),
        ),
        optional(seq('after', field('delay', $._expression))),
      ),

    fail_transition: ($) =>
      seq('fail', field('error', choice($.type_identifier, $._wildcard)), $._branches),

    output: ($) => seq('output', field('value', $._expression)),

    fail: ($) => seq('fail', field('value', $._expression)),

    // ───────────── transitions ─────────────

    _branches: ($) =>
      choice(
        seq(optional(field('guard', $.guard)), $._action),
        field('branches', $.branches),
      ),

    guard: ($) => seq('if', field('condition', $._expression)),

    branches: ($) => seq('{', repeat1($.branch), optional($.else_branch), '}'),

    branch: ($) => seq(field('guard', $.guard), $._action),

    else_branch: ($) => seq('else', $._action),

    // `ignore` takes an event and does nothing. The compiler accepts it only in an `on`
    // transition (RB0026); the grammar accepts it in any branch, as it does other misplaced
    // items the compiler rejects.
    _action: ($) =>
      choice(seq('->', field('target', $.target), optional($._effects)), $._effects, $.ignore),

    ignore: (_) => 'ignore',

    target: ($) => seq(field('state', $.type_identifier), repeat(seq('.', field('state', $.type_identifier)))),

    _effects: ($) => repeat1(field('effect', $._effect)),

    _effect: ($) => choice($.set, $.do, $.send, $.emit, $.reply, $.spawn, $.stop),

    // After a comma in a `set` or `do` list, a lower-case name continues the list, as in the
    // compiler, even `spawn`, `stop` or `children`, which after a trailing comma could also
    // start the next effect or machine item.
    set: ($) => prec.right(seq('set', commaSep1($.assignment))),

    assignment: ($) =>
      seq(field('variable', choice($.identifier, $._contextual_name)), '=', field('value', $._expression)),

    do: ($) => prec.right(seq('do', commaSep1(alias($._action_call, $.call)))),

    _action_call: ($) =>
      seq(field('function', choice($.identifier, $._contextual_name)), field('arguments', $.arguments)),

    _contextual_name: ($) =>
      choice(
        alias('spawn', $.identifier),
        alias('stop', $.identifier),
        alias('children', $.identifier),
      ),

    send: ($) =>
      seq(
        'send',
        field('to', $.identifier),
        optional(choice(seq('[', field('key', $._expression), ']'), seq('.', $._wildcard))),
        '.',
        field('event', $.type_identifier),
        optional(field('fields', $.record_body)),
      ),

    emit: ($) =>
      seq('emit', field('event', $.type_identifier), optional(field('fields', $.record_body))),

    // `reply pending done [value]` or `reply pending fail error`, to the pending call a
    // variable holds or, in the step that takes it, to `call`.
    reply: ($) =>
      seq(
        'reply',
        field('call', choice($.identifier, alias('call', $.implicit_name))),
        choice(
          seq('done', optional(field('value', $._expression))),
          seq('fail', field('value', $._expression)),
        ),
      ),

    spawn: ($) =>
      seq(
        'spawn',
        field('collection', $.identifier),
        '[',
        field('key', $._expression),
        ']',
        '=',
        field('machine', $.type_identifier),
        field('arguments', $.arguments),
      ),

    stop: ($) =>
      seq('stop', field('collection', $.identifier), '[', field('key', $._expression), ']'),

    record_body: ($) => seq('{', commaSep($.field_value), '}'),

    field_value: ($) => seq(field('name', $.identifier), ':', field('value', $._expression)),

    call: ($) => seq(field('function', $.identifier), field('arguments', $.arguments)),

    arguments: ($) => seq('(', commaSep($._expression), ')'),

    // ───────────── expressions ─────────────
    //
    // One hidden rule per precedence level, as in the EBNF, so that the operand of `not` is a
    // comparison or tighter and comparisons do not chain.

    _expression: ($) => choice($._and_level, alias($.or_expression, $.binary_expression)),

    or_expression: ($) =>
      seq(field('left', $._expression), field('operator', 'or'), field('right', $._and_level)),

    _and_level: ($) => choice($._not_level, alias($.and_expression, $.binary_expression)),

    and_expression: ($) =>
      seq(field('left', $._and_level), field('operator', 'and'), field('right', $._not_level)),

    _not_level: ($) => choice($._compare_level, $.not_expression),

    not_expression: ($) => seq(field('operator', 'not'), field('operand', $._compare_level)),

    _compare_level: ($) => choice($._add_level, alias($.compare_expression, $.binary_expression)),

    compare_expression: ($) =>
      seq(
        field('left', $._add_level),
        field('operator', choice('==', '!=', '<', '<=', '>', '>=')),
        field('right', $._add_level),
      ),

    _add_level: ($) => choice($._mul_level, alias($.add_expression, $.binary_expression)),

    add_expression: ($) =>
      seq(field('left', $._add_level), field('operator', choice('+', '-')), field('right', $._mul_level)),

    _mul_level: ($) => choice($._unary_level, alias($.mul_expression, $.binary_expression)),

    mul_expression: ($) =>
      seq(field('left', $._mul_level), field('operator', '*'), field('right', $._unary_level)),

    _unary_level: ($) => choice($._postfix_level, $.negation),

    negation: ($) => seq(field('operator', '-'), field('operand', $._postfix_level)),

    _postfix_level: ($) => choice($._primary, $.field_expression),

    field_expression: ($) =>
      seq(field('value', $._postfix_level), '.', field('field', $.identifier)),

    _primary: ($) =>
      choice(
        $.integer,
        $.float,
        $.duration,
        $.string,
        $.boolean,
        $.call,
        $.identifier,
        $._implicit_name,
        $.enum_value,
        $.record_value,
        $.parenthesized_expression,
      ),

    // The names in scope inside a transition: the current event, an invoke's result and its
    // error, the call being taken, and a spawned child's key. `event`, `error` and `call` are
    // keywords; `result` and `key` are not reserved, so outside an expression they are
    // ordinary names.
    _implicit_name: ($) =>
      choice(
        alias('event', $.implicit_name),
        alias('error', $.implicit_name),
        alias('result', $.implicit_name),
        alias('call', $.implicit_name),
        alias('key', $.implicit_name),
      ),

    // `CardDeclined { reason: "expired" }`, or a bare `GatewayUnavailable` with no fields.
    // After a bare name, a `{` is read as a block of branches where one can follow (in an
    // `after` delay), which is why a record value with fields there must be parenthesised.
    enum_value: ($) =>
      prec(1, seq(field('type', $.type_identifier), '.', field('variant', $.type_identifier))),

    record_value: ($) =>
      choice(
        prec(1, field('type', $.type_identifier)),
        seq(field('type', $.type_identifier), field('fields', $.record_body)),
      ),

    parenthesized_expression: ($) => seq('(', $._expression, ')'),

    boolean: (_) => choice('true', 'false'),

    // ───────────── tokens ─────────────

    _name: ($) => choice($.type_identifier, $.identifier),

    identifier: (_) => /[a-z][A-Za-z0-9]*/,

    type_identifier: (_) => /[A-Z][A-Za-z0-9]*/,

    integer: (_) => /[0-9]+/,

    float: (_) => /[0-9]+\.[0-9]+/,

    duration: (_) => /[0-9]+(ms|s|m|h)/,

    string: ($) =>
      seq('"', repeat(choice($.string_content, $.escape_sequence)), token.immediate('"')),

    // Higher precedence than the comments, so that `"// x"` is a string.
    string_content: (_) => token.immediate(prec(1, /[^"\\\n]+/)),

    escape_sequence: (_) => token.immediate(/\\["\\nt]/),

    // `//` comments, `////` included, and `///` doc comments. The two regexes never match the
    // same line in full, so the longest match decides.
    comment: (_) => token(choice(/\/\/([^\/\n][^\n]*)?/, /\/\/\/\/[^\n]*/)),

    doc_comment: (_) => token(/\/\/\/([^\/\n][^\n]*)?/),
  },
});
