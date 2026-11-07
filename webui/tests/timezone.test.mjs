import assert from 'node:assert/strict'
import test from 'node:test'
import {
  mergeAppConfigWithExisting,
  sanitizeConfigForSave,
  sanitizeTemplate,
} from '../src/utils/config.ts'
import { isValidTimezoneId } from '../src/utils/timezone.ts'

test('timezone survives saving unrelated settings and timezone-only templates', () => {
  const saved = sanitizeConfigForSave({
    debug: true,
    templates: {
      regional: { packages: ['com.example.app'], timezone: 'America/New_York' },
    },
    apps: [{ package: 'com.example.other@999', timezone: ' Asia/Tokyo ' }],
  })
  assert.equal(saved.templates.regional.timezone, 'America/New_York')
  assert.equal(saved.apps[0].timezone, 'Asia/Tokyo')
  assert.deepEqual(saved.templates.regional.packages, ['com.example.app'])
})

test('clearing an app timezone removes its previous value after merge and save', () => {
  const merged = mergeAppConfigWithExisting(
    { package: 'com.example.app', timezone: 'America/New_York', model: 'example' },
    { package: 'com.example.app', timezone: undefined },
  )
  const saved = sanitizeConfigForSave({ apps: [merged] })
  assert.equal(Object.hasOwn(saved.apps[0], 'timezone'), false)
  assert.equal(saved.apps[0].model, 'example')
  assert.equal(Object.hasOwn(sanitizeTemplate({ timezone: '  ' }), 'timezone'), false)
})

test('IANA regions, UTC and blank are accepted; offsets, paths and markers are rejected', () => {
  for (const id of ['', '  ', 'UTC', 'GMT', ' America/New_York ', 'Asia/Tokyo',
    'America/Argentina/Buenos_Aires', 'Etc/GMT+5']) {
    assert.equal(isValidTimezoneId(id), true, id)
  }
  for (const id of ['GMT+08:00', '+08:00', '__EMPTY__', '__DELETE__', '../Asia/Tokyo',
    '/etc/localtime', 'Asia//Tokyo', 'Asia/Tokyo\0', 'Asia/东京']) {
    assert.equal(isValidTimezoneId(id), false, id)
  }
})
