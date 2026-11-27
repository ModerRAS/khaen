import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  CAPABILITY_GAPS,
  annotateGap,
  annotateGapById,
  type GapId
} from '../src/api/capabilityGaps.ts'

const IDS: GapId[] = [
  'volumeMaster',
  'volumeLaneGain',
  'layoutFolds',
  'routingMirror',
  'deviceInputs',
  'encoding',
  'overviewPlan',
  'overviewDevice',
  'calibrationMeasure',
  'systemRestart',
  'systemDaemonRestart',
  'speakerTest'
]

test('every open gap has a task id and is not the already-wired mode or AES67 item', () => {
  assert.deepEqual(Object.keys(CAPABILITY_GAPS).sort(), [...IDS].sort())
  for (const id of IDS) {
    const task = CAPABILITY_GAPS[id].task
    assert.ok(task === 'R' || task === 'C' || task === 'R/IO', id)
    assert.equal(CAPABILITY_GAPS[id].id, id)
  }
  assert.equal(CAPABILITY_GAPS.encoding.task, 'R')
  assert.equal(CAPABILITY_GAPS.calibrationMeasure.task, 'C')
  assert.equal(CAPABILITY_GAPS.routingMirror.task, 'R/IO')
  assert.equal(CAPABILITY_GAPS.deviceInputs.task, 'R/IO')
})

test('annotateGap appends the task once in both locales', () => {
  assert.equal(annotateGap('no master volume', 'R', 'zh-CN'), 'no master volume （任务 R）')
  assert.equal(annotateGap('no master volume', 'R', 'en-US'), 'no master volume (task R)')
  assert.equal(annotateGap(null, 'C', 'zh-CN'), '（任务 C）')
  const once = annotateGapById('mirror', 'routingMirror', 'en-US')
  assert.equal(once, 'mirror (task R/IO)')
  assert.equal(annotateGap(once, 'R/IO', 'en-US'), once)
})
