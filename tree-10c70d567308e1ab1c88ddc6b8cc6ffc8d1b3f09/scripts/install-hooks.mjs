#!/usr/bin/env bun
// Installation is explicit; commits execute the copied launcher, never this file.
import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, readFileSync, realpathSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

function output(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`${command} ${args.join(' ')} failed: ${result.stderr || result.error}`);
  return result.stdout.trim();
}
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;

try {
  const root = output('git', ['rev-parse', '--show-toplevel']);
  process.chdir(root);
  const common = resolve(output('git', ['rev-parse', '--git-common-dir']));
  const configuredHooks = spawnSync('git', ['config', '--get', 'core.hooksPath'], { encoding: 'utf8' });
  if (configuredHooks.status === 0) {
    throw new Error(`configured core.hooksPath preserved (${configuredHooks.stdout.trim()}); integrate existing hooks explicitly before repository-local bootstrap`);
  }
  const hook = resolve(output('git', ['rev-parse', '--git-path', 'hooks/pre-commit']));
  const marker = '# Velnor isolated-index pre-commit';
  if (existsSync(hook) && !readFileSync(hook, 'utf8').includes(marker)) {
    throw new Error(`existing hook preserved at ${hook}; integrate it explicitly before installing Velnor hooks`);
  }
  const config = Bun.TOML.parse(readFileSync('mise.toml', 'utf8'));
  const bunVersion = config.tools['aqua:oven-sh/bun'];
  const prekVersion = config.tools['aqua:j178/prek'];
  if (typeof bunVersion !== 'string' || typeof prekVersion !== 'string') throw new Error('Bun and prek must have exact version pins in mise.toml');
  if (Bun.version !== bunVersion) throw new Error(`run bootstrap with pinned Bun ${bunVersion}, found ${Bun.version}`);
  const mise = realpathSync(output('sh', ['-c', 'command -v mise']));
  const prek = realpathSync(output(mise, ['which', 'prek']));
  if (!output(prek, ['--version']).startsWith(`prek ${prekVersion} `)) throw new Error(`install pinned prek ${prekVersion}: mise install`);
  const rustup = realpathSync(output('sh', ['-c', 'command -v rustup']));
  const git = realpathSync(output('sh', ['-c', 'command -v git']));
  const destination = join(common, 'velnor-hooks');
  mkdirSync(destination, { recursive: true });
  writeFileSync(join(destination, 'empty-global.toml'), '# Hook execution does not inherit global mise policy.\n');
  const launcher = join(destination, process.platform === 'win32' ? 'snapshot.exe' : 'snapshot');
  // Embed the pinned Bun runtime and disable every runtime configuration loader.
  // A worktree bunfig preload must not run before index isolation exists.
  const candidate = `${launcher}.${process.pid}`;
  output(process.execPath, ['build', '--compile', '--no-compile-autoload-dotenv', '--no-compile-autoload-bunfig', '--no-compile-autoload-tsconfig', '--no-compile-autoload-package-json', 'scripts/pre-commit-snapshot.mjs', '--outfile', candidate]);
  // Bun 1.4.0 changes its signed Mach-O while embedding the script. Re-sign the
  // locally generated executable, then replace the old inode atomically.
  if (process.platform === 'darwin') output('/usr/bin/codesign', ['--force', '--sign', '-', candidate]);
  renameSync(candidate, launcher);
  writeFileSync(join(destination, 'installation.json'), JSON.stringify({
    bun: process.execPath, bunVersion, prek, prekVersion, mise, rustup, git,
    miseData: process.env.MISE_DATA_DIR || join(process.env.HOME, '.local/share/mise'),
    rustupHome: process.env.RUSTUP_HOME || join(process.env.HOME, '.rustup'),
  }, null, 2));
  mkdirSync(dirname(hook), { recursive: true });
  writeFileSync(hook, `#!/bin/sh\n${marker}\n# No worktree configuration or runtime preload is read before isolation.\nif [ ! -x ${quote(launcher)} ]; then\n  echo 'Velnor hooks: launcher missing; run mise install && mise run bootstrap' >&2\n  exit 1\nfi\nset --\nfor key in HOME USER LOGNAME TMPDIR TEMP TMP SystemRoot COMSPEC PATHEXT HTTPS_PROXY HTTP_PROXY ALL_PROXY NO_PROXY SSL_CERT_FILE SSL_CERT_DIR GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES; do\n  eval 'present=\${'"$key"'+set}'\n  if [ "$present" = set ]; then\n    eval 'value=\${'"$key"'-}'\n    set -- "$@" "$key=$value"\n  fi\ndone\nexec /usr/bin/env -i "$@" ${quote(launcher)} ${quote(join(destination, 'installation.json'))}\n`);
  chmodSync(hook, 0o755);
  console.log(`Installed isolated-index prek hook: ${hook}`);
} catch (error) {
  console.error(`Velnor hooks: ${error.message}`);
  process.exit(1);
}
