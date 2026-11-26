import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  formatClock,
  formatDecimal,
  formatInteger,
  formatPercent,
  formatTimestamp,
  shortenId,
  toByteUnit,
  toRateUnit
} from '../src/utils/format.ts'

test('formatInteger localizes grouping and rejects invalid values', () => {
  assert.equal(formatInteger(1234567, 'en-US'), '1,234,567')
  assert.equal(formatInteger(0, 'en-US'), '0')
  assert.equal(formatInteger(-42, 'en-US'), '-42')
  assert.equal(formatInteger(null, 'en-US'), null)
  assert.equal(formatInteger(undefined, 'en-US'), null)
  assert.equal(formatInteger(Number.NaN, 'en-US'), null)
  assert.equal(formatInteger(Number.POSITIVE_INFINITY, 'en-US'), null)
})

test('formatDecimal caps fraction digits', () => {
  assert.equal(formatDecimal(48, 2, 'en-US'), '48')
  assert.equal(formatDecimal(2.769, 2, 'en-US'), '2.77')
  assert.equal(formatDecimal(0.123456, 3, 'en-US'), '0.123')
  assert.equal(formatDecimal(null, 2, 'en-US'), null)
})

test('formatPercent renders ratios as percentages', () => {
  assert.equal(formatPercent(0.0001, 'en-US'), '0.01%')
  assert.equal(formatPercent(0.25, 'en-US'), '25%')
  assert.equal(formatPercent(1, 'en-US'), '100%')
  assert.equal(formatPercent(null, 'en-US'), null)
  assert.equal(formatPercent(Number.NaN, 'en-US'), null)
})

test('formatTimestamp returns null for missing/invalid ISO strings', () => {
  const formatted = formatTimestamp('2026-11-26T03:45:12.000Z', 'en-US')
  assert.ok(formatted !== null && formatted.includes('2026'))
  assert.equal(formatTimestamp(null, 'en-US'), null)
  assert.equal(formatTimestamp('', 'en-US'), null)
  assert.equal(formatTimestamp('not-a-date', 'en-US'), null)
  assert.equal(formatTimestamp(42, 'en-US'), null)
})

test('formatClock formats local epoch milliseconds', () => {
  const formatted = formatClock(Date.parse('2026-11-26T03:45:12.000Z'), 'en-US')
  assert.ok(formatted !== null && formatted.length > 0)
  assert.equal(formatClock(null, 'en-US'), null)
  assert.equal(formatClock(Number.NaN, 'en-US'), null)
})

test('shortenId keeps short values and middle-elides long ones', () => {
  assert.equal(shortenId(null), null)
  assert.equal(shortenId(''), null)
  assert.equal(shortenId('short'), 'short')
  assert.equal(shortenId('12345678901234'), '12345678\u20261234')
  assert.equal(shortenId('8bcc0f0b1d2e3f405162738495a6b7c8', 8, 4), '8bcc0f0b\u2026b7c8')
  assert.equal(shortenId('8bcc0f0b1d2e3f405162738495a6b7c8', 12, 8), '8bcc0f0b1d2e\u202695a6b7c8')
})

test('toByteUnit picks 1024-based units', () => {
  assert.deepEqual(toByteUnit(0), { value: 0, unitKey: 'unit.bytes' })
  assert.deepEqual(toByteUnit(1023), { value: 1023, unitKey: 'unit.bytes' })
  assert.deepEqual(toByteUnit(1024), { value: 1, unitKey: 'unit.kib' })
  assert.deepEqual(toByteUnit(1024 * 1024), { value: 1, unitKey: 'unit.mib' })
  const gib = toByteUnit(2973696000)
  assert.ok(gib !== null)
  assert.equal(gib.unitKey, 'unit.gib')
  assert.ok(Math.abs(gib.value - 2.7693) < 0.001)
  assert.equal(toByteUnit(-1), null)
  assert.equal(toByteUnit(null), null)
})

test('toRateUnit picks Hz / kHz / MHz', () => {
  assert.deepEqual(toRateUnit(999), { value: 999, unitKey: 'unit.hz' })
  assert.deepEqual(toRateUnit(1000), { value: 1, unitKey: 'unit.khz' })
  assert.deepEqual(toRateUnit(48000), { value: 48, unitKey: 'unit.khz' })
  assert.deepEqual(toRateUnit(96000), { value: 96, unitKey: 'unit.khz' })
  assert.deepEqual(toRateUnit(1000000), { value: 1, unitKey: 'unit.mhz' })
  assert.equal(toRateUnit(0), null)
  assert.equal(toRateUnit(-48000), null)
  assert.equal(toRateUnit(null), null)
})
