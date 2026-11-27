// Maps and generic types in the viewer: the values of simulate mode's forms, declarations in
// source form, and the names and handles a type reference mentions.

import { describe, expect, it } from 'vitest'
import type { Ir, TypeDecl } from '../src/ir.ts'
import { refMachine, typeDeclText, typeNamesOf } from '../src/model.ts'
import { checkValue, sampleValue } from '../src/values.ts'

const span = { file: 0, line: 1, col: 1, endLine: 1, endCol: 1 }
const field = (name: string, type: string) => ({ name, type, sensitive: false, doc: '' })
const types: Record<string, TypeDecl> = {
  UserId: { kind: 'alias', params: [], type: 'string', doc: '', span },
  Page: { kind: 'record', params: ['T'], fields: [field('items', 'T[]'), field('next', 'string?')], doc: '', span },
  Pair: { kind: 'record', params: ['A', 'B'], fields: [field('first', 'A'), field('second', 'B')], doc: '', span },
  Dict: { kind: 'alias', params: ['T'], type: 'map<string, T>', doc: '', span },
  First: { kind: 'alias', params: ['T'], type: 'T', doc: '', span },
  Peers: { kind: 'alias', params: [], type: 'ref Peer[]', doc: '', span },
}
const ir = { irVersion: '0.3', sources: [], types, functions: {}, actions: {}, tasks: {}, machines: {} } as unknown as Ir

describe('sample values', () => {
  it('are an empty object for a map, and the substituted record for an instance', () => {
    expect(sampleValue(ir, 'map<string, int>')).toEqual({})
    expect(sampleValue(ir, 'Dict<Page<int>>')).toEqual({})
    expect(sampleValue(ir, 'Page<int>')).toEqual({ items: [], next: null })
    expect(sampleValue(ir, 'Pair<First<int>, Pair<bool, UserId>>')).toEqual({ first: 0, second: { first: false, second: '' } })
    for (const t of ['Page<int>', 'Pair<First<int>, Pair<bool, UserId>>', 'Dict<int>']) expect(checkValue(ir, t, sampleValue(ir, t))).toEqual([])
  })
})

describe('checking values', () => {
  it('checks each value of a map, by its key', () => {
    expect(checkValue(ir, 'map<UserId, int>', JSON.parse('{"a": 1, "__proto__": "x"}'), 'scores')).toEqual([
      'scores["__proto__"]: expected an integer within ±9007199254740991, got "x"',
    ])
    expect(checkValue(ir, 'Dict<int>', [1])).toEqual(['value: expected a map: an object with a member per key (map<string, int>), got a list'])
    expect(checkValue(ir, 'Dict<int>?', null)).toEqual([])
  })

  it('checks an instance against its substituted fields', () => {
    expect(checkValue(ir, 'Page<Pair<string, int>>', { items: [{ first: 'a', second: 'b' }], next: null, x: 1 }, 'page')).toEqual([
      'page.items[0].second: expected an integer within ±9007199254740991, got "b"',
      'page.x: not a field',
    ])
    expect(checkValue(ir, 'First<bool>', 1)).toEqual(['value: expected true or false, got 1'])
  })
})

describe('type references', () => {
  it('show a generic declaration with its parameters', () => {
    expect(typeDeclText('Page', types.Page!)).toBe('type Page<T> { items: T[], next: string? }')
    expect(typeDeclText('Dict', types.Dict!)).toBe('type Dict<T> = map<string, T>')
    expect(typeDeclText('UserId', types.UserId!)).toBe('type UserId = string')
  })

  it('name every declared type they mention', () => {
    expect(typeNamesOf('map<UserId, Page<Order>>?')).toEqual(['UserId', 'Page', 'Order'])
    expect(typeNamesOf('LineItem[]?')).toEqual(['LineItem'])
    expect(typeNamesOf('ref Peer')).toEqual([])
    expect(typeNamesOf('not a type')).toEqual([])
  })

  it('give the machine of a handle, but not of a map of handles', () => {
    expect(refMachine(ir, 'ref Peer[]?')).toBe('Peer')
    expect(refMachine(ir, 'Peers?')).toBe('Peer')
    expect(refMachine(ir, 'map<string, ref Peer>')).toBeNull()
    expect(refMachine(ir, 'Dict<int>')).toBeNull()
  })
})
