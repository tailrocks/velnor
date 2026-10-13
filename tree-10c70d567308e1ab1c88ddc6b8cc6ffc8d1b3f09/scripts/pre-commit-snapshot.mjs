#!/usr/bin/env bun
// Copied into Git's common directory at bootstrap. Never import worktree code.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, readlinkSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, delimiter } from 'node:path';

const installationDirectory = dirname(process.argv[2]);
let lock;
const cleanup = () => { if (lock) rmSync(lock, { recursive: true, force: true }); };
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => { cleanup(); process.exit(1); });

try {
  const tools = JSON.parse(readFileSync(join(installationDirectory, 'installation.json'), 'utf8'));
  for (const name of ['bun', 'prek', 'mise', 'rustup', 'git']) {
    if (!existsSync(tools[name])) throw new Error(`pinned ${name} missing; run mise install && mise run bootstrap`);
  }
  // Retain only host transport settings. Inherited mise/task/skip/compiler overrides
  // cannot silently alter the staged validation policy.
  const env = {};
  for (const key of ['HOME', 'USER', 'LOGNAME', 'TMPDIR', 'TEMP', 'TMP', 'SystemRoot', 'COMSPEC', 'PATHEXT', 'HTTPS_PROXY', 'HTTP_PROXY', 'ALL_PROXY', 'NO_PROXY', 'SSL_CERT_FILE', 'SSL_CERT_DIR']) {
    if (process.env[key] !== undefined) env[key] = process.env[key];
  }
  env.PATH = [...new Set([dirname(tools.git), dirname(tools.mise), dirname(tools.bun), dirname(tools.prek), dirname(tools.rustup), '/usr/bin', '/bin', '/usr/sbin', '/sbin'])].join(delimiter);
  env.RUSTUP_HOME = tools.rustupHome;
  env.MISE_DATA_DIR = tools.miseData;
  env.MISE_LOCKED = '1';
  env.MISE_AUTO_INSTALL = '0';
  env.MISE_TASK_RUN_AUTO_INSTALL = '0';
  env.MISE_GLOBAL_CONFIG_FILE = join(installationDirectory, 'empty-global.toml');
  env.MISE_SYSTEM_CONFIG_DIR = join(installationDirectory, 'empty-system-config');
  env.PREK_HOME = join(installationDirectory, 'prek-cache');
  env.CARGO_HOME = join(installationDirectory, 'cargo-cache');
  env.GIT_CONFIG_GLOBAL = '/dev/null';
  env.GIT_CONFIG_SYSTEM = '/dev/null';
  env.GIT_CONFIG_NOSYSTEM = '1';
  mkdirSync(env.CARGO_HOME, { recursive: true });
  const gitEnv = { ...env };
  // Git supplies an alternate index for partial/pathspec commits. Resolve it before
  // changing directory, and never expose that index to the snapshot commands.
  for (const key of ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES']) {
    if (process.env[key] !== undefined) gitEnv[key] = process.env[key];
  }
  function command(executable, args, cwd, environment = env, inherit = false) {
    const result = spawnSync(executable, args, { cwd, env: environment, encoding: 'utf8', stdio: inherit ? 'inherit' : 'pipe', maxBuffer: 64 * 1024 * 1024 });
    if (result.status !== 0) throw new Error(`${executable} ${args.join(' ')} failed${result.stderr ? `: ${result.stderr.trim()}` : ''}${result.error ? `: ${result.error.message}` : ''}`);
    return result.stdout?.trimEnd() || '';
  }
  const original = process.cwd();
  const root = command(tools.git, ['rev-parse', '--show-toplevel'], original, gitEnv);
  const tree = command(tools.git, ['write-tree'], original, gitEnv);
  const head = command(tools.git, ['rev-parse', '--verify', 'HEAD'], original, gitEnv);
  const identity = createHash('sha256').update(realpathSync(root)).digest('hex').slice(0, 24);
  const temporary = join(installationDirectory, `snapshot-${identity}`);
  mkdirSync(temporary, { recursive: true });
  const candidateLock = join(temporary, 'lock');
  try { mkdirSync(candidateLock); }
  catch { throw new Error(`snapshot already locked: ${candidateLock}; wait for the active commit, or recover this lock only after verifying its owner has stopped`); }
  lock = candidateLock;
  writeFileSync(join(lock, 'owner'), `${process.pid}\n`);
  const snapshot = join(temporary, 'repository');
  if (!existsSync(join(snapshot, '.git'))) command(tools.git, ['-c', 'init.templateDir=', 'clone', '--quiet', '--shared', '--no-checkout', root, snapshot], original);
  command(tools.git, ['config', 'core.hooksPath', '/dev/null'], snapshot);
  command(tools.git, ['update-ref', 'HEAD', head], snapshot);
  command(tools.git, ['read-tree', '--reset', '-u', tree], snapshot);
  command(tools.git, ['clean', '-ffdx'], snapshot);
  const stages = command(tools.git, ['ls-files', '--stage', '-z'], snapshot);
  const entries = stages.split('\0').filter(Boolean).map((entry) => {
    const tab = entry.indexOf('\t');
    const [mode, hash, stage] = entry.slice(0, tab).split(' ');
    if (stage !== '0' || !['100644', '100755', '120000'].includes(mode)) throw new Error('unmerged entries and submodules are unsupported by staged validation');
    return { mode, hash, path: entry.slice(tab + 1) };
  });
  const objectFormat = command(tools.git, ['rev-parse', '--show-object-format'], snapshot);
  function rawBlobHash(entry) {
    const path = join(snapshot, entry.path);
    const data = entry.mode === '120000' ? readlinkSync(path, { encoding: 'buffer' }) : readFileSync(path);
    return createHash(objectFormat).update(`blob ${data.length}\0`).update(data).digest('hex');
  }
  // Git attributes may request EOL or encoding conversion. Checks consume raw
  // index blobs, never a global filter's output or a transformed checkout.
  for (const entry of entries) {
    if (rawBlobHash(entry) === entry.hash) continue;
    if (entry.mode === '120000') throw new Error(`symlink differs from its staged blob: ${entry.path}`);
    const result = spawnSync(tools.git, ['cat-file', 'blob', entry.hash], { cwd: snapshot, env, maxBuffer: 64 * 1024 * 1024 });
    if (result.status !== 0) throw new Error(`cannot materialize staged blob ${entry.path}`);
    writeFileSync(join(snapshot, entry.path), result.stdout);
    chmodSync(join(snapshot, entry.path), entry.mode === '100755' ? 0o755 : 0o644);
    if (rawBlobHash(entry) !== entry.hash) throw new Error(`staged blob verification failed: ${entry.path}`);
  }
  const inside = (path) => { const part = relative(snapshot, path); return part !== '..' && !part.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) && !isAbsolute(part); };
  const manifests = [];
  function inspect(directory) {
    for (const name of readdirSync(directory)) {
      if (directory === snapshot && name === '.git') continue;
      const path = join(directory, name);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) {
        if (!existsSync(path) || !inside(realpathSync(path))) throw new Error(`staged symlink escapes snapshot or has no staged target: ${relative(snapshot, path)}`);
      } else if (stat.isDirectory()) inspect(path);
      else if (name === 'Cargo.toml' || (['config', 'config.toml'].includes(name) && directory.endsWith('/.cargo'))) manifests.push(path);
    }
  }
  inspect(snapshot);
  // Cargo path dependencies and workspace membership may otherwise escape a
  // disposable checkout and consume unrelated untracked files from its parent.
  function inspectPaths(value, directory, key = '') {
    if (Array.isArray(value)) { for (const item of value) inspectPaths(item, directory, key); }
    else if (value && typeof value === 'object') { for (const [field, item] of Object.entries(value)) inspectPaths(item, directory, field); }
    else if (typeof value === 'string' && ['path', 'paths', 'members', 'default-members', 'exclude'].includes(key)) {
      if (!inside(resolve(directory, value))) throw new Error(`Cargo ${key} escapes staged snapshot: ${value}`);
    }
  }
  for (const manifest of manifests) inspectPaths(Bun.TOML.parse(readFileSync(manifest, 'utf8')), dirname(manifest));
  const policy = Bun.TOML.parse(readFileSync(join(snapshot, 'mise.toml'), 'utf8'));
  if (policy.tools?.['aqua:oven-sh/bun'] !== tools.bunVersion || policy.tools?.['aqua:j178/prek'] !== tools.prekVersion) throw new Error('staged hook tool pins differ from bootstrap; install the staged versions and run mise run bootstrap');
  env.MISE_CEILING_PATHS = temporary;
  env.MISE_TRUSTED_CONFIG_PATHS = snapshot;
  const toolchainFile = join(snapshot, 'rust-toolchain.toml');
  if (existsSync(toolchainFile)) {
    const channel = Bun.TOML.parse(readFileSync(toolchainFile, 'utf8')).toolchain?.channel;
    if (typeof channel !== 'string' || !/^[A-Za-z0-9_.-]+$/.test(channel)) throw new Error('staged rust-toolchain.toml requires an explicit channel');
    const shimDirectory = join(temporary, 'tool-bin');
    mkdirSync(shimDirectory, { recursive: true });
    const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
    for (const executable of ['cargo', 'rustc', 'rustfmt']) {
      const path = join(shimDirectory, executable);
      writeFileSync(path, `#!/bin/sh\nexec ${quote(tools.rustup)} run ${quote(channel)} ${executable} "$@"\n`);
      chmodSync(path, 0o755);
    }
    env.PATH = `${shimDirectory}${delimiter}${env.PATH}`;
  }
  // Cache compiled products outside source. Cargo fingerprints still validate
  // content; no successful-check status is cached. Cargo owns its build lock.
  env.CARGO_TARGET_DIR = join(temporary, 'target');
  console.log('Velnor hooks: validate staged snapshot (formatting, then Clippy)');
  command(tools.prek, ['run', '--config', join(snapshot, '.pre-commit-config.yaml'), '--all-files', '--show-diff-on-failure'], snapshot, env, true);
  for (const entry of entries) if (rawBlobHash(entry) !== entry.hash) throw new Error(`validation modified staged source: ${entry.path}`);
  if (command(tools.git, ['write-tree'], snapshot) !== tree) throw new Error('validation modified the staged snapshot index');
  if (command(tools.git, ['write-tree'], original, gitEnv) !== tree) throw new Error('index changed during validation; retry the commit');
} catch (error) {
  console.error(`Velnor hooks: ${error.message}`);
  process.exitCode = 1;
} finally {
  cleanup();
}
