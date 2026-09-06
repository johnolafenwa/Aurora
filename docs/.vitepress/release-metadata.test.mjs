import assert from 'node:assert/strict'
import test from 'node:test'

import {
  AURA_RELEASE,
  resolveImplementationCommit
} from './release-metadata.mjs'

test('the maintained documentation source identifies the 0.3.3 technical preview', () => {
  assert.deepEqual(AURA_RELEASE, {
    version: '0.3.3',
    channel: 'technical preview'
  })
})

test('AURA_DOCS_COMMIT takes precedence over GITHUB_SHA and the local checkout', () => {
  const commit = resolveImplementationCommit({
    env: {
      AURA_DOCS_COMMIT: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
      GITHUB_SHA: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'
    },
    readLocalCommit: () => 'cccccccccccccccccccccccccccccccccccccccc',
    isLocalCheckoutDirty: () => false
  })
  assert.equal(commit, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')
})

test('GITHUB_SHA is used when an explicit documentation commit is absent', () => {
  const commit = resolveImplementationCommit({
    env: { GITHUB_SHA: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' },
    readLocalCommit: () => 'cccccccccccccccccccccccccccccccccccccccc',
    isLocalCheckoutDirty: () => false
  })
  assert.equal(commit, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb')
})

test('an invalid environment value falls back to the local checkout commit', () => {
  const commit = resolveImplementationCommit({
    env: { AURA_DOCS_COMMIT: 'not-a-commit' },
    readLocalCommit: () => 'cccccccccccccccccccccccccccccccccccccccc',
    isLocalCheckoutDirty: () => false
  })
  assert.equal(commit, 'cccccccccccccccccccccccccccccccccccccccc')
})

test('a missing or invalid local checkout has an honest non-commit fallback', () => {
  assert.equal(
    resolveImplementationCommit({
      env: {},
      readLocalCommit: () => '',
      isLocalCheckoutDirty: () => false
    }),
    'local-uncommitted-checkout'
  )
  assert.equal(
    resolveImplementationCommit({
      env: {},
      readLocalCommit: () => {
        throw new Error('git unavailable')
      },
      isLocalCheckoutDirty: () => false
    }),
    'local-uncommitted-checkout'
  )
})

test('a dirty checkout never claims its committed HEAD as the rendered baseline', () => {
  assert.equal(
    resolveImplementationCommit({
      env: {},
      readLocalCommit: () => 'cccccccccccccccccccccccccccccccccccccccc',
      isLocalCheckoutDirty: () => true
    }),
    'local-uncommitted-checkout'
  )
})
