/* tslint:disable */
/* eslint-disable */

/**
 * Checks each root (a JSON array of project paths), as `roadbook check` does.
 */
export function check(host: any, config: string, roots: string): string;

/**
 * Compiles an entry (a project path) to the IR, as `roadbook export ir` does.
 */
export function compile(host: any, config: string, entry: string): string;

/**
 * The `--json` report of a usage or I/O error that stopped a check.
 */
export function fatalReport(message: string): string;

/**
 * Formats a source canonically, as `roadbook fmt` does.
 */
export function format(name: string, source: string): string;

/**
 * Generates the TypeScript module for an entry (a project path), as `roadbook gen` does.
 */
export function gen(host: any, config: string, entry: string): string;

/**
 * The compiler's version.
 */
export function version(): string;
