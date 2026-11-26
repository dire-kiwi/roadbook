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

  // Every keyword is reserved: none can be a name. `from`, `as`, `ref` and the built-in type
  // names are not, because they mean something only where no name can appear.
  reserved: {
    global: (_) => [
      'import', 'type', 'enum', 'error', 'extern', 'fn', 'action', 'task', 'machine', 'var',
      'event', 'emits', 'initial', 'state', 'final', 'parallel', 'entry', 'exit', 'on', 'after',
      'always', 'done', 'invoke', 'output', 'fail', 'if', 'else', 'set', 'do', 'send', 'emit',
      'and', 'or', 'not', 'true', 'false',
    ],
  },

  conflicts: ($) => [
    // `Role.Admin` (an enum value) against `Thing.field` (a field read of a record value):
    // told apart by the case of the name after the `.`, one token further on.
    [$.record_value, $.enum_value],
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

    _type: ($) => choice($.type_identifier, $.builtin_type, $.ref_type, $.list_type, $.optional_type),

    builtin_type: (_) => choice('string', 'int', 'float', 'bool', 'duration', 'json'),

    ref_type: ($) => seq('ref', field('machine', $.type_identifier)),

    list_type: ($) => seq(field('element', $._type), '[', ']'),

    optional_type: ($) => seq(field('type', $._type), '?'),

    // ───────────── machines ─────────────

    machine_declaration: ($) =>
      seq(
        'machine',
        field('name', $.type_identifier),
        field('parameters', $.parameters),
        optional(seq('->', field('return_type', $._type))),
        optional(field('errors', $.errors)),
        field('body', $.machine_body),
      ),

    machine_body: ($) => seq('{', repeat($._machine_item), '}'),

    _machine_item: ($) =>
      choice($.variable_declaration, $.event_declaration, $.emits_declaration, $._common_item),

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
      choice($._common_item, $.done_transition, $.invoke, $.output, $.fail),

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

    _trigger: ($) => choice($.event_pattern, $._wildcard, $.received_event),

    event_pattern: ($) => field('event', $.type_identifier),

    _wildcard: ($) => alias('*', $.wildcard),

    received_event: ($) =>
      seq(field('from', $.identifier), '.', field('event', $.type_identifier)),

    after_transition: ($) => seq('after', field('delay', $._expression), $._branches),

    always_transition: ($) => seq('always', $._branches),

    done_transition: ($) => seq('done', $._branches),

    invoke: ($) =>
      seq(
        'invoke',
        field('callee', $._name),
        field('arguments', $.arguments),
        optional(seq('as', field('alias', $.identifier))),
        '{',
        repeat1(choice($.done_transition, $.fail_transition)),
        '}',
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

    _action: ($) => choice(seq('->', field('target', $.target), optional($._effects)), $._effects),

    target: ($) => seq(field('state', $.type_identifier), repeat(seq('.', field('state', $.type_identifier)))),

    _effects: ($) => repeat1(field('effect', $._effect)),

    _effect: ($) => choice($.set, $.do, $.send, $.emit),

    set: ($) => seq('set', commaSep1($.assignment)),

    assignment: ($) => seq(field('variable', $.identifier), '=', field('value', $._expression)),

    do: ($) => seq('do', commaSep1($.call)),

    send: ($) =>
      seq(
        'send',
        field('to', $.identifier),
        '.',
        field('event', $.type_identifier),
        optional(field('fields', $.record_body)),
      ),

    emit: ($) =>
      seq('emit', field('event', $.type_identifier), optional(field('fields', $.record_body))),

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
    // error. `event` and `error` are keywords; `result` is not reserved, so outside an
    // expression it is an ordinary name.
    _implicit_name: ($) =>
      choice(
        alias('event', $.implicit_name),
        alias('error', $.implicit_name),
        alias('result', $.implicit_name),
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
