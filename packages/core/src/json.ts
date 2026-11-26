/** JSON values, and their canonical form (RFC 8785). */

export type Json = null | boolean | number | string | ReadonlyArray<Json> | { readonly [key: string]: Json }

/**
 * The canonical JSON of `value` (RFC 8785, the JSON Canonicalization Scheme): no whitespace,
 * object members sorted by name as arrays of UTF-16 code units (JavaScript's default string
 * order), strings and numbers written as `JSON.stringify` writes them. Throws on a value that
 * has no JSON form (a non-finite number, `undefined`, a function).
 */
export const canonicalJson = (value: unknown): string => {
  if (value === null) return "null"
  switch (typeof value) {
    case "boolean":
      return value ? "true" : "false"
    case "number":
      if (!Number.isFinite(value)) throw new TypeError(`canonical JSON has no form for ${value}`)
      return JSON.stringify(value)
    case "string":
      return JSON.stringify(value)
    case "object": {
      if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`
      const object = value as Record<string, unknown>
      const keys = Object.keys(object).filter((key) => object[key] !== undefined).sort()
      return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(object[key])}`).join(",")}}`
    }
    default:
      throw new TypeError(`canonical JSON has no form for a value of type ${typeof value}`)
  }
}
