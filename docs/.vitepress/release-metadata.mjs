import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

export const AURA_RELEASE = Object.freeze({
  version: '0.3.3',
  channel: 'technical preview'
})

const COMMIT_PATTERN = /^[0-9a-f]{7,40}$/i
const REPOSITORY_ROOT = fileURLToPath(new URL('../..', import.meta.url))

function readCheckoutCommit() {
  return execFileSync('git', ['rev-parse', '--verify', 'HEAD^{commit}'], {
    cwd: REPOSITORY_ROOT,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore']
  }).trim()
}

function checkoutIsDirty() {
  return execFileSync(
    'git',
    ['status', '--porcelain', '--untracked-files=normal'],
    {
      cwd: REPOSITORY_ROOT,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore']
    }
  ).trim().length > 0
}

/**
 * Resolve the implementation baseline without writing a hash back into source.
 * Release/archive builds should set AURA_DOCS_COMMIT explicitly. GitHub builds
 * inherit GITHUB_SHA. A clean local checkout uses its current committed HEAD;
 * a dirty checkout or exported source archive without Git reports that it is
 * an uncommitted local build instead of inventing a commit.
 */
export function resolveImplementationCommit({
  env = process.env,
  readLocalCommit = readCheckoutCommit,
  isLocalCheckoutDirty = checkoutIsDirty
} = {}) {
  for (const candidate of [env.AURA_DOCS_COMMIT, env.GITHUB_SHA]) {
    const value = candidate?.trim()
    if (value && COMMIT_PATTERN.test(value)) {
      return value.toLowerCase()
    }
  }

  try {
    if (isLocalCheckoutDirty()) {
      return 'local-uncommitted-checkout'
    }
    const local = readLocalCommit().trim()
    if (COMMIT_PATTERN.test(local)) {
      return local.toLowerCase()
    }
  } catch {
    // A source archive need not contain Git metadata. The explicit fallback is
    // more honest than stamping it with the release-building checkout.
  }
  return 'local-uncommitted-checkout'
}
