//! The compiler's diagnostic codes: `RB01xx` names, `RB02xx` structure, `RB03xx` types,
//! `RB04xx` analyses, `RB05xx` properties (failing `check` lines, which only `check`
//! evaluates) and `RB09xx` warnings. Each is listed with an example in the "Diagnostic
//! codes" table of `docs/design/language.md`, and each has a failing chart in
//! `tests/fail/`. The syntax codes, `RB00xx`, are in `roadbook_syntax::codes`.

use roadbook_syntax::diagnostic::Code;

// ───────────── names ─────────────

/// A type name that is not declared or imported.
pub const UNKNOWN_TYPE: Code = Code(101);
/// An `invoke` of a task or machine that is not declared or imported.
pub const UNKNOWN_INVOKE: Code = Code(102);
/// A call of a function that is not declared or imported.
pub const UNKNOWN_FUNCTION: Code = Code(103);
/// A `do` of an action that is not declared or imported.
pub const UNKNOWN_ACTION: Code = Code(104);
/// A variable that is not a parameter or `var` of the machine.
pub const UNKNOWN_VARIABLE: Code = Code(105);
/// An `on` naming an event the machine does not declare.
pub const UNKNOWN_EVENT: Code = Code(106);
/// Two top-level declarations with the same name in one compilation, or an event with the
/// name of a type, error or machine.
pub const DUPLICATE_DECLARATION: Code = Code(107);
/// Two fields, parameters, variants, variables, events or sibling states with the same name,
/// or a name imported twice into one file.
pub const DUPLICATE_NAME: Code = Code(108);
/// A declared name used as something it is not, such as an action in `invoke`.
pub const WRONG_KIND: Code = Code(109);
/// `event`, `result`, `error`, `call` or `key` where it is not in scope.
pub const IMPLICIT_NAME_OUT_OF_SCOPE: Code = Code(110);
/// An import of a file that cannot be read.
pub const IMPORT_NOT_FOUND: Code = Code(111);
/// An import of a name the imported file does not declare.
pub const IMPORT_NOT_DECLARED: Code = Code(112);
/// An enum value naming a variant the enum does not have.
pub const UNKNOWN_VARIANT: Code = Code(113);
/// A `set` of a machine parameter. Parameters are read-only.
pub const ASSIGN_TO_PARAMETER: Code = Code(114);

/// A `send name.E` or `on name.E` whose `name` is neither an `invoke … as name` in scope nor a
/// parameter or variable, or an `invoke name.call(…)` or `on name.ended` whose `name` is not a
/// parameter or variable.
pub const UNKNOWN_ADDRESS: Code = Code(115);

/// A `send name.E` of an event the receiving machine does not declare with `event`.
pub const NOT_ACCEPTED: Code = Code(116);

/// An `emit E` of an event the machine does not declare with `emits`, or an `on name.E` of an
/// event the sending machine does not.
pub const NOT_EMITTED: Code = Code(117);

/// An `invoke x.name(…)` of a call `x`'s machine does not declare, an `on name` of a call the
/// machine does not declare, or a `call name` type naming no call of the machine.
pub const UNKNOWN_CALL: Code = Code(118);

/// The built-in error `Unanswered` where it cannot be: in a `!` list, as a type or a value, or
/// in a `fail` outcome of an invoke that is not a call.
pub const UNANSWERED_MISUSED: Code = Code(119);
/// `attempt` in a retry's delay, in a machine with a parameter or variable named `attempt`.
pub const ATTEMPT_SHADOWED: Code = Code(120);
/// A `spawn`, `stop`, `send name[key]`, `send name.*`, `on name.done` or `on name.fail` through a
/// name that is not a `children` collection, or a `send name.E` to a collection with no key.
pub const NOT_A_COLLECTION: Code = Code(121);
/// `key`, the implicit name of a spawned child's key, as the name of a machine parameter,
/// variable, `as` name or collection.
pub const KEY_AS_NAME: Code = Code(122);
/// A state path in a `check` that names no state of the machine, or names a history state.
pub const UNKNOWN_CHECK_STATE: Code = Code(123);
/// An `on name.ended` through an `invoke … as name` child or a `children` collection, whose
/// ends are their outcomes, not through a `ref`.
pub const ENDED_NOT_A_PEER: Code = Code(124);

// ───────────── structure ─────────────

/// A transition target that does not resolve to a state.
pub const UNKNOWN_TARGET: Code = Code(201);
/// A state with children, or a machine, with no `initial`; or a machine with no states.
pub const MISSING_INITIAL: Code = Code(202);
/// An `initial` that is not a child, is written twice, or is in a state without children.
pub const BAD_INITIAL: Code = Code(203);
/// An `invoke` that leaves a declared error of the task or machine unhandled, or, for a call,
/// `Unanswered`.
pub const UNHANDLED_ERROR: Code = Code(204);
/// The same trigger written twice in one state or one `invoke`.
pub const DUPLICATE_TRIGGER: Code = Code(205);
/// A second `invoke` in one state.
pub const SECOND_INVOKE: Code = Code(206);
/// A `fail E` outcome, or a `retry … on E`, for an error the task or machine does not
/// declare.
pub const UNDECLARED_ERROR_OUTCOME: Code = Code(207);
/// `output` or `fail` outside a top-level final state, or more than one in a final state.
pub const MISPLACED_RESULT: Code = Code(208);
/// A top-level final state with no `output` or `fail` in a machine that returns a value.
pub const MISSING_RESULT: Code = Code(209);
/// A final state with children, or a parallel state with none.
pub const STATE_KIND_MISMATCH: Code = Code(210);
/// A transition or `invoke` in a final state, which is left as soon as it is entered.
pub const ACTIVITY_IN_FINAL_STATE: Code = Code(211);
/// A `done` transition in a state with no child states, which can never be taken.
pub const DONE_WITHOUT_CHILDREN: Code = Code(212);

/// A `send` to an invoked child where the child is not running: in the invoking state's own
/// `entry` or `exit`, in its invoke's outcomes, or in a transition that leaves the invoking
/// state.
pub const CHILD_NOT_RUNNING: Code = Code(213);

/// `invoke task(…) as name` or `invoke x.call(…) as name`: only an invoked machine has a name
/// to send to.
pub const ALIAS_ON_TASK: Code = Code(214);

/// `retry` on the invoke of a state with child states, which cannot be written out as
/// `Attempt` and `Backoff` children.
pub const RETRY_WITH_CHILDREN: Code = Code(215);

/// `retry` on an invoke with `as`: each attempt is a new child, which a name cannot address
/// during the waits.
pub const RETRY_WITH_ALIAS: Code = Code(216);

/// A `retry` that retries nothing (`retry 0`, a task or machine with no declared errors), or
/// names an error twice.
pub const RETRY_NOTHING: Code = Code(217);

/// A retried error handled by a `fail *` that also handles an error that is not retried.
pub const RETRY_SHARED_OUTCOME: Code = Code(218);

/// `history H` or `deep history H` in a state with no child states, or in a final state.
pub const HISTORY_WITHOUT_CHILDREN: Code = Code(219);
/// A `spawn` of a machine other than its collection's.
pub const SPAWN_MACHINE_MISMATCH: Code = Code(220);
/// Several targets of which some have the leading dot of an internal transition and some do not.
pub const MIXED_TARGETS: Code = Code(221);
/// Several targets that are not each in a different region of a parallel state.
pub const TARGETS_CLASH: Code = Code(222);

// ───────────── types ─────────────

/// A value of the wrong type.
pub const TYPE_MISMATCH: Code = Code(301);
/// A call, `do`, `invoke` with the wrong number of arguments.
pub const ARGUMENT_COUNT: Code = Code(302);
/// An operator applied to operands it is not defined on.
pub const BAD_OPERANDS: Code = Code(303);
/// A field that the record, error or event does not have.
pub const UNKNOWN_FIELD: Code = Code(304);
/// A record or error value that leaves out a field.
pub const MISSING_FIELD: Code = Code(305);
/// A field read from a value that has no fields: a scalar, an optional, or a record, error
/// or event declared with none.
pub const NO_FIELDS: Code = Code(306);
/// `result` in the `done` of a task or machine that returns nothing.
pub const NO_RESULT: Code = Code(307);
/// `error` in a `fail *` that covers more than one error, or none.
pub const AMBIGUOUS_ERROR: Code = Code(308);
/// `event` in an `on *` transition, or used whole in an `on name` transition that takes a
/// call, where it is read only through its fields.
pub const UNTYPED_EVENT: Code = Code(309);
/// `output` in a machine that returns nothing.
pub const OUTPUT_WITHOUT_RETURN: Code = Code(310);
/// A `fail` value that is not one of the machine's declared errors.
pub const FAIL_NOT_DECLARED: Code = Code(311);
/// An `int` or `duration` literal beyond ±(2^53 − 1), which the IR cannot carry exactly.
pub const LITERAL_OUT_OF_RANGE: Code = Code(312);
/// A type alias that refers to itself.
pub const ALIAS_CYCLE: Code = Code(313);
/// A type made optional twice, such as `string??`.
pub const DOUBLE_OPTIONAL: Code = Code(314);

/// A `send name.E`, `on name.E` or `invoke name.call(…)` through a value that is not a `ref`:
/// another type, or a `ref` that is optional or a list; or an `on name.ended` through one that
/// is none of `ref M`, `ref M?` and `ref M[]`.
pub const NOT_A_REF: Code = Code(315);

/// A `ref` in a field of a record, error or event, in a parameter of a `call`, or in what a
/// function, task or machine returns.
pub const REF_NOT_ALLOWED: Code = Code(316);

/// A pending call type `call name` anywhere but as the type of a machine variable (optional,
/// in a list or an alias, a parameter, a field, a result), or a variable of it with an
/// initial value.
pub const CALL_TYPE_NOT_ALLOWED: Code = Code(317);

/// A `reply x …` where `x` is neither `call` nor a variable holding a pending call.
pub const NOT_A_PENDING_CALL: Code = Code(318);

/// A `reply` that does not answer as its call is declared: `done` with a value for a call that
/// returns nothing or without one for a call that returns a value, or `fail` with a value that
/// is not one of the call's declared errors.
pub const BAD_REPLY: Code = Code(319);
/// A value read from a sensitive field, parameter or variable (or computed from one by an
/// operator) going where a trace shows it:
/// to a variable, parameter or field that is not sensitive, or to an `output`, a `fail`, a
/// call's reply or an `after` delay.
pub const SENSITIVE_VALUE: Code = Code(320);

/// A value whose type has sensitive fields going where `json` is expected, in a place a trace
/// shows.
pub const SENSITIVE_INTO_JSON: Code = Code(321);
/// A `children` key type that is not `string`, `int` or an enum, or an alias of one.
pub const BAD_KEY_TYPE: Code = Code(322);

/// A `map` whose key type is not `string` or an alias of it.
pub const MAP_KEY: Code = Code(323);

/// A generic type without its type arguments or with the wrong number of them, or type
/// arguments on a type that takes none.
pub const TYPE_ARGUMENT_COUNT: Code = Code(324);

/// A type parameter its declaration does not use.
pub const UNUSED_TYPE_PARAMETER: Code = Code(325);

/// A generic type that refers to itself, directly or through other types, with other type
/// arguments than its own parameters.
pub const IRREGULAR_RECURSION: Code = Code(326);

/// A record value of a generic type whose type arguments cannot be told from the expected
/// type or the field values.
pub const UNINFERRED_TYPE_ARGUMENT: Code = Code(327);

// ───────────── analyses ─────────────

/// A cycle of unguarded `always` transitions, or an unguarded `always` with no target.
pub const ALWAYS_CYCLE: Code = Code(401);
/// A machine that invokes itself without waiting for an event or an `after`.
pub const UNBOUNDED_RECURSION: Code = Code(402);
/// A read of a variable that might not be assigned yet (definite assignment).
pub const UNASSIGNED_READ: Code = Code(403);

// ───────────── properties ─────────────

/// A `check never` whose states can be active (all together, for several).
pub const NEVER_ACTIVE: Code = Code(501);
/// A `check reachable` whose state no path reaches, whatever the guards.
pub const UNREACHABLE_CHECK_STATE: Code = Code(502);
/// A `check S only after T` where `S` can be active before `T` has been.
pub const ACTIVE_BEFORE: Code = Code(503);
/// A `check can finish` where a configuration can be reached from which no top-level final
/// state can be.
pub const CANNOT_FINISH: Code = Code(504);

// ───────────── warnings ─────────────

/// A state that cannot be reached from the machine's initial state.
pub const UNREACHABLE_STATE: Code = Code(901);
/// A non-final state with no transition out of it.
pub const NO_WAY_OUT: Code = Code(902);
/// An event, function, action or task that nothing uses.
pub const UNUSED_DECLARATION: Code = Code(903);
/// An `invoke` with no `after` on its state or any state containing it. Off by default.
pub const INVOKE_WITHOUT_TIMEOUT: Code = Code(904);

/// An `emits E` that no `emit E` of the machine sends.
pub const NEVER_EMITTED: Code = Code(905);

/// An `on name.E` where the sending machine declares `emits E` but never emits it.
pub const RECEIVES_NOTHING: Code = Code(906);

/// An `emits E` that no `on name.E` in the checked charts receives.
pub const NEVER_RECEIVED: Code = Code(907);

/// A `call` that no `on name` of its machine takes.
pub const CALL_NEVER_TAKEN: Code = Code(908);

/// A variable of type `call name` that no `reply` answers.
pub const CALL_NEVER_REPLIED: Code = Code(909);
/// A history state that no transition targets.
pub const UNUSED_HISTORY: Code = Code(910);
/// A `children` collection that no `spawn` fills (warning).
pub const NEVER_SPAWNED: Code = Code(911);
/// A machine whose checks were not evaluated: it has more configurations than the budget.
pub const NOT_CHECKED: Code = Code(912);
/// A transition that leaves a parallel state and enters it again, starting regions over that
/// it does not target.
pub const REGIONS_RESET: Code = Code(913);

/// Every code defined here, in order.
pub const ALL: &[Code] = &[
    UNKNOWN_TYPE,
    UNKNOWN_INVOKE,
    UNKNOWN_FUNCTION,
    UNKNOWN_ACTION,
    UNKNOWN_VARIABLE,
    UNKNOWN_EVENT,
    DUPLICATE_DECLARATION,
    DUPLICATE_NAME,
    WRONG_KIND,
    IMPLICIT_NAME_OUT_OF_SCOPE,
    IMPORT_NOT_FOUND,
    IMPORT_NOT_DECLARED,
    UNKNOWN_VARIANT,
    ASSIGN_TO_PARAMETER,
    UNKNOWN_ADDRESS,
    NOT_ACCEPTED,
    NOT_EMITTED,
    UNKNOWN_CALL,
    UNANSWERED_MISUSED,
    ATTEMPT_SHADOWED,
    NOT_A_COLLECTION,
    KEY_AS_NAME,
    UNKNOWN_CHECK_STATE,
    ENDED_NOT_A_PEER,
    UNKNOWN_TARGET,
    MISSING_INITIAL,
    BAD_INITIAL,
    UNHANDLED_ERROR,
    DUPLICATE_TRIGGER,
    SECOND_INVOKE,
    UNDECLARED_ERROR_OUTCOME,
    MISPLACED_RESULT,
    MISSING_RESULT,
    STATE_KIND_MISMATCH,
    ACTIVITY_IN_FINAL_STATE,
    DONE_WITHOUT_CHILDREN,
    CHILD_NOT_RUNNING,
    ALIAS_ON_TASK,
    RETRY_WITH_CHILDREN,
    RETRY_WITH_ALIAS,
    RETRY_NOTHING,
    RETRY_SHARED_OUTCOME,
    HISTORY_WITHOUT_CHILDREN,
    SPAWN_MACHINE_MISMATCH,
    MIXED_TARGETS,
    TARGETS_CLASH,
    TYPE_MISMATCH,
    ARGUMENT_COUNT,
    BAD_OPERANDS,
    UNKNOWN_FIELD,
    MISSING_FIELD,
    NO_FIELDS,
    NO_RESULT,
    AMBIGUOUS_ERROR,
    UNTYPED_EVENT,
    OUTPUT_WITHOUT_RETURN,
    FAIL_NOT_DECLARED,
    LITERAL_OUT_OF_RANGE,
    ALIAS_CYCLE,
    DOUBLE_OPTIONAL,
    NOT_A_REF,
    REF_NOT_ALLOWED,
    CALL_TYPE_NOT_ALLOWED,
    NOT_A_PENDING_CALL,
    BAD_REPLY,
    SENSITIVE_VALUE,
    SENSITIVE_INTO_JSON,
    BAD_KEY_TYPE,
    MAP_KEY,
    TYPE_ARGUMENT_COUNT,
    UNUSED_TYPE_PARAMETER,
    IRREGULAR_RECURSION,
    UNINFERRED_TYPE_ARGUMENT,
    ALWAYS_CYCLE,
    UNBOUNDED_RECURSION,
    UNASSIGNED_READ,
    NEVER_ACTIVE,
    UNREACHABLE_CHECK_STATE,
    ACTIVE_BEFORE,
    CANNOT_FINISH,
    UNREACHABLE_STATE,
    NO_WAY_OUT,
    UNUSED_DECLARATION,
    INVOKE_WITHOUT_TIMEOUT,
    NEVER_EMITTED,
    RECEIVES_NOTHING,
    NEVER_RECEIVED,
    CALL_NEVER_TAKEN,
    CALL_NEVER_REPLIED,
    UNUSED_HISTORY,
    NEVER_SPAWNED,
    NOT_CHECKED,
    REGIONS_RESET,
];

#[cfg(test)]
mod tests {
    /// Every code here is in `ALL` and in the table in `language.md`, after the syntax codes.
    #[test]
    fn codes_are_documented() {
        let defined: Vec<String> = include_str!("codes.rs")
            .lines()
            .filter(|l| l.starts_with("pub const") && l.contains("= Code("))
            .filter_map(|l| {
                l.split_once("= Code(")
                    .map(|(_, n)| n.trim_end_matches(");").parse::<u16>().unwrap())
            })
            .map(|n| format!("RB{n:04}"))
            .collect();
        let all: Vec<String> = super::ALL.iter().map(|c| c.to_string()).collect();
        assert_eq!(defined, all);
        let doc = include_str!("../../../docs/design/language.md");
        let table: Vec<String> = doc
            .lines()
            .filter_map(|l| l.strip_prefix("| `RB"))
            .map(|l| format!("RB{}", &l[..4]))
            .filter(|c| !c.starts_with("RB00"))
            .collect();
        assert_eq!(defined, table);
    }
}
