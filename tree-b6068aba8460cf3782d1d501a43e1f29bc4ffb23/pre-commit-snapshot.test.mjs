import { afterEach, expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const repository = resolve(import.meta.dir, '..');
const versions = Bun.TOML.parse(readFileSync(join(repository, 'mise.toml'), 'utf8')).tools;
const rustChannel = Bun.TOML.parse(readFileSync(join(repository, 'rust-toolchain.toml'), 'utf8')).toolchain.channel;
const roots = [];
const bun = process.execPath;
const mise = spawnSync('sh', ['-c', 'command -v mise'], { encoding: 'utf8' }).stdout.trim();
const cleanEnv = { ...process.env, PATH: `${dirname(mise)}:${dirname(bun)}:${process.env.PATH}` };

function run(root, command, args, extra = {}) {
  return spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...cleanEnv, MISE_TRUSTED_CONFIG_PATHS: root, ...extra }, maxBuffer: 16 * 1024 * 1024 });
}
function ok(root, command, args, extra) {
  const result = run(root, command, args, extra);
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')}: ${result.stdout}\n${result.stderr}`);
  return result.stdout.trim();
}
const git = (root, ...args) => ok(root, 'git', args);
function fixture() {
  const parent = mkdtempSync(join(tmpdir(), 'velnor-hooks-test-'));
  roots.push(parent);
  const root = join(parent, 'repository with spaces');
  mkdirSync(join(root, 'scripts'), { recursive: true });
  for (const file of ['scripts/install-hooks.mjs', 'scripts/pre-commit-snapshot.mjs', '.pre-commit-config.yaml', 'mise.lock']) copyFileSync(join(repository, file), join(root, file));
  const trace = join(parent, 'trace');
  writeFileSync(join(root, 'mise.toml'), `[tools]\n"aqua:oven-sh/bun" = ${JSON.stringify(versions['aqua:oven-sh/bun'])}\n"aqua:j178/prek" = ${JSON.stringify(versions['aqua:j178/prek'])}\n[settings]\nlockfile = true\nidiomatic_version_file_enable_tools = []\n[tasks.fmt]\nrun = "sh scripts/validate.sh fmt"\n[tasks.lint]\nrun = "sh scripts/validate.sh lint"\n`);
  writeFileSync(join(root, 'scripts/validate.sh'), `echo "$1" >> '${trace}'\ncase "$1" in\n fmt) ! grep -q BAD_FMT source.rs ;;\n lint) ! grep -q BAD_LINT source.rs ;;\nesac\n`);
  writeFileSync(join(root, 'source.rs'), 'GOOD\n');
  writeFileSync(join(root, '.gitignore'), 'ignored.txt\n');
  git(root, 'init', '-q');
  git(root, 'config', 'user.name', 'Hook Fixture');
  git(root, 'config', 'user.email', 'fixture@example.invalid');
  git(root, 'add', '.');
  git(root, '-c', 'core.hooksPath=/dev/null', 'commit', '-q', '-s', '-m', 'fixture\n\nCo-authored-by: Codex <codex@openai.com>');
  ok(root, bun, ['--no-env-file', 'scripts/install-hooks.mjs']);
  const hook = resolve(root, git(root, 'rev-parse', '--git-path', 'hooks/pre-commit'));
  return { root, hook, trace, parent };
}
function state(root) {
  const files = [];
  function walk(dir) {
    for (const name of readdirSync(dir).sort()) {
      if (dir === root && name === '.git') continue;
      const path = join(dir, name);
      const stat = lstatSync(path);
      if (stat.isSymbolicLink()) files.push([path, 'link', readlinkSync(path)]);
      else if (stat.isDirectory()) walk(path);
      else files.push([path, stat.mode, readFileSync(path).toString('hex')]);
    }
  }
  walk(root);
  return JSON.stringify([files, git(root, 'diff', '--binary'), git(root, 'diff', '--cached', '--binary'), git(root, 'stash', 'list')]);
}
function check(f, succeeds, extra = {}) {
  const before = state(f.root);
  const result = run(f.root, f.hook, [], extra);
  if (result.error || result.signal || (result.status === 0) !== succeeds) throw new Error(`hook exit ${result.status}, signal ${result.signal}, error ${result.error}\n${result.stdout}\n${result.stderr}`);
  expect(state(f.root)).toBe(before);
  return result;
}
const trace = (f) => existsSync(f.trace) ? readFileSync(f.trace, 'utf8').trim().split('\n') : [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { force: true, recursive: true }); });

test('bootstrap is idempotent and successful checks run formatting before Clippy', () => {
  const f = fixture();
  ok(f.root, bun, ['scripts/install-hooks.mjs']);
  writeFileSync(join(f.root, 'notes.txt'), 'untracked');
  writeFileSync(join(f.root, 'ignored.txt'), 'ignored');
  check(f, true);
  expect(trace(f)).toEqual(['fmt', 'lint']);
}, 30000);

for (const [violation, expected] of [['BAD_FMT', ['fmt']], ['BAD_LINT', ['fmt', 'lint']]]) {
  test(`staged ${violation} rejected even when unstaged content restores HEAD`, () => {
    const f = fixture();
    writeFileSync(join(f.root, 'source.rs'), `${violation}\n`);
    git(f.root, 'add', 'source.rs');
    writeFileSync(join(f.root, 'source.rs'), 'GOOD\n');
    check(f, false);
    expect(trace(f)).toEqual(expected);
  }, 30000);
}

for (const file of ['mise.toml', '.pre-commit-config.yaml', 'scripts/validate.sh', 'scripts/pre-commit-snapshot.mjs']) {
  test(`unstaged ${file} cannot suppress staged failure`, () => {
    const f = fixture();
    writeFileSync(join(f.root, 'source.rs'), 'BAD_FMT\n');
    git(f.root, 'add', 'source.rs');
    const path = join(f.root, file);
    if (file === 'mise.toml') writeFileSync(path, readFileSync(path, 'utf8').replaceAll('sh scripts/validate.sh fmt', 'true'));
    else if (file === '.pre-commit-config.yaml') writeFileSync(path, readFileSync(path, 'utf8').replaceAll('mise run fmt', 'true'));
    else writeFileSync(path, 'true\n');
    check(f, false);
    expect(trace(f)).toEqual(['fmt']);
  }, 30000);
}

for (const file of ['untracked.txt', 'ignored.txt']) {
  test(`${file} cannot provide missing staged dependency`, () => {
    const f = fixture();
    writeFileSync(join(f.root, 'scripts/validate.sh'), `test -f ${file}\n`);
    git(f.root, 'add', 'scripts/validate.sh');
    writeFileSync(join(f.root, file), 'dependency');
    check(f, false);
  }, 30000);
}

test('runtime preload, inherited mise config, skip and compiler overrides cannot suppress failure', () => {
  const f = fixture();
  writeFileSync(join(f.root, 'source.rs'), 'BAD_FMT\n');
  git(f.root, 'add', 'source.rs');
  writeFileSync(join(f.root, 'bunfig.toml'), 'preload = ["./bypass.mjs"]\n');
  writeFileSync(join(f.root, 'bypass.mjs'), 'process.exit(0);\n');
  writeFileSync(join(f.root, '.env'), 'PREK_SKIP=fmt,clippy\n');
  check(f, false, { PREK_SKIP: 'fmt,clippy', SKIP: 'fmt,clippy', MISE_ENV: 'bypass', MISE_CONFIG_FILE: '/dev/null', RUSTC_WRAPPER: '/usr/bin/true', BUN_OPTIONS: '--preload ./bypass.mjs' });
  expect(trace(f)).toEqual(['fmt']);
}, 30000);

for (const changed of ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'rustfmt.toml', '.cargo/config.toml', '.github-gen/config.toml', 'scripts/new-helper.sh']) {
  test(`configuration-only ${changed} still runs both checks`, () => {
    const f = fixture();
    mkdirSync(dirname(join(f.root, changed)), { recursive: true });
    writeFileSync(join(f.root, changed), changed === 'rust-toolchain.toml' ? `[toolchain]\nchannel = ${JSON.stringify(rustChannel)}\n` : '# fixture\n');
    git(f.root, 'add', changed);
    check(f, true);
    expect(trace(f)).toEqual(['fmt', 'lint']);
  }, 30000);
}

test('alternate index is the validation source', () => {
  const f = fixture();
  const index = join(f.parent, 'partial-index');
  ok(f.root, 'git', ['read-tree', 'HEAD'], { GIT_INDEX_FILE: index });
  writeFileSync(join(f.root, 'source.rs'), 'BAD_FMT\n');
  ok(f.root, 'git', ['add', 'source.rs'], { GIT_INDEX_FILE: index });
  writeFileSync(join(f.root, 'source.rs'), 'GOOD\n');
  check(f, false, { GIT_INDEX_FILE: index });
}, 30000);

test('normal git commit invokes installed hook and rejects invalid staged snapshot', () => {
  const f = fixture();
  writeFileSync(join(f.root, 'source.rs'), 'BAD_LINT\n');
  git(f.root, 'add', 'source.rs');
  const head = git(f.root, 'rev-parse', 'HEAD');
  const result = run(f.root, 'git', ['commit', '-s', '-m', 'invalid\n\nCo-authored-by: Codex <codex@openai.com>']);
  expect(result.status).not.toBe(0);
  expect(git(f.root, 'rev-parse', 'HEAD')).toBe(head);
  expect(trace(f)).toEqual(['fmt', 'lint']);
}, 30000);

test('linked worktree uses its own index and shared installed hook', () => {
  const f = fixture();
  const linked = join(f.parent, 'linked worktree');
  git(f.root, 'worktree', 'add', '--detach', linked, 'HEAD');
  writeFileSync(join(linked, 'source.rs'), 'BAD_FMT\n');
  git(linked, 'add', 'source.rs');
  check({ ...f, root: linked }, false);
  expect(trace(f)).toEqual(['fmt']);
}, 30000);

test('staged deletion cannot leave HEAD file available in snapshot', () => {
  const f = fixture();
  git(f.root, 'rm', 'source.rs');
  writeFileSync(join(f.root, 'scripts/validate.sh'), 'test -f source.rs\n');
  git(f.root, 'add', 'scripts/validate.sh');
  check(f, false);
}, 30000);

test('escaping symlinks and Cargo path dependencies fail before checks', () => {
  const f = fixture();
  symlinkSync(f.parent, join(f.root, 'outside'));
  git(f.root, 'add', 'outside');
  expect(check(f, false).stderr).toContain('symlink escapes');
  git(f.root, 'rm', '-f', 'outside');
  writeFileSync(join(f.root, 'Cargo.toml'), '[dependencies]\nescape = { path = "../outside" }\n');
  git(f.root, 'add', 'Cargo.toml');
  expect(check(f, false).stderr).toContain('escapes staged snapshot');
  expect(trace(f)).toEqual([]);
}, 30000);

test('missing pinned dependency produces actionable failure', () => {
  const f = fixture();
  const config = join(f.root, '.git/velnor-hooks/installation.json');
  const installed = JSON.parse(readFileSync(config, 'utf8'));
  installed.prek = join(f.parent, 'missing-prek');
  writeFileSync(config, JSON.stringify(installed));
  expect(check(f, false).stderr).toContain('run mise install && mise run bootstrap');
}, 30000);

test('foreign pre-commit hook is preserved and bootstrap refuses destructive replacement', () => {
  const f = fixture();
  writeFileSync(f.hook, '#!/bin/sh\necho existing\n');
  const result = run(f.root, bun, ['scripts/install-hooks.mjs']);
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain('existing hook preserved');
  expect(readFileSync(f.hook, 'utf8')).toContain('echo existing');
}, 30000);

test('real Rust formatting and Clippy violations fail; corrected warm snapshot works offline', () => {
  const f = fixture();
  mkdirSync(join(f.root, 'src'));
  writeFileSync(join(f.root, 'Cargo.toml'), '[package]\nname = "hook-fixture"\nversion = "0.1.0"\nedition = "2024"\n');
  copyFileSync(join(repository, 'rust-toolchain.toml'), join(f.root, 'rust-toolchain.toml'));
  writeFileSync(join(f.root, 'src/lib.rs'), 'pub fn valid() -> bool { true }\n');
  ok(f.root, 'cargo', ['generate-lockfile', '--offline']);
  const config = join(f.root, 'mise.toml');
  const hooks = join(f.root, '.pre-commit-config.yaml');
  writeFileSync(hooks, readFileSync(hooks, 'utf8').replaceAll('always_run: true', 'always_run: true\n        verbose: true'));
  writeFileSync(config, readFileSync(config, 'utf8')
    .replace('sh scripts/validate.sh fmt', 'cargo fmt --all --check')
    .replace('sh scripts/validate.sh lint', 'cargo clippy --workspace --all-targets --locked --offline -- -D warnings'));
  git(f.root, 'add', '.');
  writeFileSync(join(f.root, 'src/lib.rs'), 'pub fn valid() -> bool {\n    true\n}\n');
  const formatting = check(f, false);
  expect(formatting.stdout).toContain('Rust formatting');
  expect(formatting.stdout).not.toContain('Rust Clippy');
  writeFileSync(join(f.root, 'src/lib.rs'), 'pub fn valid() -> bool {\n    true == true\n}\n');
  git(f.root, 'add', 'src/lib.rs');
  writeFileSync(join(f.root, 'src/lib.rs'), 'pub fn valid() -> bool {\n    true\n}\n');
  const lint = check(f, false);
  expect(lint.stdout).toContain('Rust Clippy');
  expect(lint.stdout).toContain('equal expressions');
  git(f.root, 'add', 'src/lib.rs');
  check(f, true, { HTTP_PROXY: 'http://127.0.0.1:9', HTTPS_PROXY: 'http://127.0.0.1:9', ALL_PROXY: 'http://127.0.0.1:9' });
  const warm = check(f, true, { HTTP_PROXY: 'http://127.0.0.1:9', HTTPS_PROXY: 'http://127.0.0.1:9', ALL_PROXY: 'http://127.0.0.1:9' });
  expect(warm.stdout).not.toContain('Checking hook-fixture');
}, 60000);

test('global core.hooksPath is never overwritten or installed into', () => {
  const f = fixture();
  const hooks = join(f.parent, 'global-hooks');
  mkdirSync(hooks);
  const global = join(f.parent, 'global-gitconfig');
  writeFileSync(global, `[core]\n  hooksPath = ${hooks}\n`);
  const result = run(f.root, bun, ['scripts/install-hooks.mjs'], { GIT_CONFIG_GLOBAL: global });
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain('core.hooksPath preserved');
  expect(readdirSync(hooks)).toEqual([]);
  expect(readFileSync(global, 'utf8')).toContain(hooks);
}, 30000);

test('global Git smudge filter cannot replace the staged content being checked', () => {
  const f = fixture();
  writeFileSync(join(f.root, '.gitattributes'), 'source.rs filter=fix\n');
  writeFileSync(join(f.root, 'source.rs'), 'BAD_FMT\n');
  git(f.root, 'add', '.gitattributes', 'source.rs');
  const global = join(f.parent, 'global-gitconfig');
  writeFileSync(global, '[filter "fix"]\n  smudge = sed s/BAD_FMT/GOOD/\n  clean = cat\n');
  check(f, false, { GIT_CONFIG_GLOBAL: global });
  expect(trace(f)).toEqual(['fmt']);
}, 30000);

test('EOL attributes cannot transform raw staged blob bytes', () => {
  const f = fixture();
  writeFileSync(join(f.root, '.gitattributes'), 'source.rs text eol=crlf\n');
  writeFileSync(join(f.root, 'scripts/validate.sh'), 'test "$(wc -c < source.rs | tr -d " ")" = 5\n');
  git(f.root, 'add', '.gitattributes', 'scripts/validate.sh');
  check(f, true);
  check(f, true);
}, 30000);

test('stable snapshots reject overlapping ownership and remove untracked leftovers', () => {
  const f = fixture();
  check(f, true);
  const hooks = join(f.root, '.git/velnor-hooks');
  const snapshot = join(hooks, readdirSync(hooks).find((name) => name.startsWith('snapshot-')));
  mkdirSync(join(snapshot, 'lock'));
  expect(check(f, false).stderr).toContain('snapshot already locked');
  expect(existsSync(join(snapshot, 'lock'))).toBe(true);
  rmSync(join(snapshot, 'lock'), { recursive: true });
  writeFileSync(join(snapshot, 'repository/untracked.txt'), 'previous untracked dependency');
  writeFileSync(join(f.root, 'scripts/validate.sh'), 'test ! -f untracked.txt\n');
  git(f.root, 'add', 'scripts/validate.sh');
  check(f, true);
}, 30000);

test('rustup selection takes precedence over a rogue cargo beside Git', () => {
  const f = fixture();
  const rogue = join(f.parent, 'rogue-bin');
  mkdirSync(rogue);
  const actualGit = ok(f.root, 'sh', ['-c', 'command -v git']);
  symlinkSync(actualGit, join(rogue, 'git'));
  writeFileSync(join(rogue, 'cargo'), '#!/bin/sh\nexit 99\n', { mode: 0o755 });
  const installedPath = join(f.root, '.git/velnor-hooks/installation.json');
  const installed = JSON.parse(readFileSync(installedPath, 'utf8'));
  installed.git = join(rogue, 'git');
  writeFileSync(installedPath, JSON.stringify(installed));
  copyFileSync(join(repository, 'rust-toolchain.toml'), join(f.root, 'rust-toolchain.toml'));
  writeFileSync(join(f.root, 'scripts/validate.sh'), `cargo --version | grep -Fq ${JSON.stringify(rustChannel)}\n`);
  git(f.root, 'add', 'rust-toolchain.toml', 'scripts/validate.sh');
  check(f, true);
}, 30000);

test('actual check aggregate enforces timestamp order and stops after formatting or lint failure', () => {
  const f = fixture();
  const policy = Bun.TOML.parse(readFileSync(join(repository, 'mise.toml'), 'utf8'));
  const references = policy.tasks.check.run;
  expect(references.slice(0, 3).map((entry) => entry.task)).toEqual(['fmt', 'lint', 'test']);
  const names = references.map((entry) => entry.task);
  writeFileSync(join(f.root, 'timed.mjs'), `import {appendFileSync, existsSync, readFileSync} from 'node:fs';\nconst name=process.argv[2];\nconst prior=existsSync('times')?readFileSync('times','utf8').trim().split('\\n').map(JSON.parse):[];\nconst required={lint:'fmt',test:'lint'}[name];\nif(required&&!prior.some(x=>x.name===required&&x.phase==='end')) process.exit(4);\nappendFileSync('times',JSON.stringify({name,phase:'start',time:Date.now()})+'\\n');\nawait Bun.sleep(40);\nappendFileSync('times',JSON.stringify({name,phase:'end',time:Date.now()})+'\\n');\nif(existsSync('fail-'+name))process.exit(1);\n`);
  writeFileSync(join(f.root, 'mise.toml'), `[tasks.check]\nrun = [${references.map((entry) => `{task = ${JSON.stringify(entry.task)}}`).join(', ')}]\n` + names.map((name) => `[tasks.${JSON.stringify(name)}]\nrun = ${JSON.stringify(`${bun} timed.mjs ${name}`)}\n`).join(''));
  for (const failure of [null, 'fmt', 'lint']) {
    for (const file of ['times', 'fail-fmt', 'fail-lint']) rmSync(join(f.root, file), { force: true });
    if (failure) writeFileSync(join(f.root, `fail-${failure}`), '');
    const result = run(f.root, mise, ['run', 'check']);
    expect(result.status === 0).toBe(failure === null);
    const events = readFileSync(join(f.root, 'times'), 'utf8').trim().split('\n').map(JSON.parse);
    const expected = failure === 'fmt' ? ['fmt'] : failure === 'lint' ? ['fmt', 'lint'] : names;
    expect(events.filter((event) => event.phase === 'start').map((event) => event.name)).toEqual(expected);
    for (let i = 0; i < events.length; i += 2) {
      expect(events[i + 1].phase).toBe('end');
      expect(events[i + 1].time).toBeGreaterThan(events[i].time);
      if (i > 0) expect(events[i].time).toBeGreaterThanOrEqual(events[i - 1].time);
    }
  }
}, 30000);
