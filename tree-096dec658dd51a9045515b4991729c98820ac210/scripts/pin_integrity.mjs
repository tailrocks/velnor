#!/usr/bin/env node
// Version-pin integrity gate.
//
// A version pin is only a pin if something fails when it drifts. The
// `actions/runner` protocol pin in crates/velnor-runner/src/protocol.rs is the
// model used here: the pin is not written down, it is asserted. Every other pin
// in this repository is enforced by this script, because two silent failure
// modes had already occurred without anyone noticing:
//
//   1. A Renovate `customManagers` entry whose `matchStrings` match nothing in
//      the files it targets. Renovate logs no error for this; it simply opens no
//      pull request. Dead automation is externally indistinguishable from
//      automation that has nothing to do, which is how a defective Rust
//      toolchain pin sat through compiler-correctness backports here without a
//      single update PR.
//
//   2. The same version pinned in several files with nothing comparing the
//      copies, so the local, build and job environments quietly run different
//      tools: a `mise.toml` declaration that its adjacent lockfile no longer
//      agrees with (mise resolves tools from the lock, so a stale lock keeps the
//      old version), the same tool at two versions in two mise configs, or a
//      version literal repeated in a Dockerfile assertion.
//
// The property enforced is: every custom manager extracts a real pin, and every
// pin has exactly one authoritative site that all its copies agree with.
//
// Run: mise run pin-integrity

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const repoRoot = execFileSync('git', ['rev-parse', '--show-toplevel'], {
  encoding: 'utf8',
}).trim();

const renovateConfigPath =
  process.env.PIN_INTEGRITY_RENOVATE_CONFIG ??
  resolve(repoRoot, 'renovate.json');
const pinManifestPath =
  process.env.PIN_INTEGRITY_MANIFEST ??
  resolve(repoRoot, 'config/version-pins.json');

const failures = [];
const fail = (message) => failures.push(message);

const readRepoFile = (path) => readFileSync(resolve(repoRoot, path), 'utf8');

function compile(label, pattern) {
  try {
    return new RegExp(pattern, 'g');
  } catch (error) {
    fail(`${label}: ${JSON.stringify(pattern)} is not a valid regular expression: ${error.message}`);
    return null;
  }
}

function section(title, header, rows) {
  console.log(title);
  console.log('');
  if (rows.length === 0) {
    console.log('(nothing to report)');
    console.log('');
    return;
  }
  const widths = header.map((cell, column) =>
    Math.max(cell.length, ...rows.map((row) => row[column].length)),
  );
  const line = (cells) =>
    cells
      .map((cell, index) => cell.padEnd(widths[index]))
      .join('  ')
      .trimEnd();
  console.log(line(header));
  console.log(widths.map((width) => '-'.repeat(width)).join('  '));
  for (const row of rows) console.log(line(row));
  console.log('');
}

const trackedFiles = execFileSync('git', ['ls-files'], {
  cwd: repoRoot,
  encoding: 'utf8',
  maxBuffer: 64 * 1024 * 1024,
})
  .split('\n')
  .filter(Boolean);

// Renovate reads a `managerFilePatterns` entry delimited by `/` as a regular
// expression over the repository-relative path, and anything else as a glob.
// Brace alternatives matter here: the mise manager's own defaults are written
// as `**/{,.}mise{,.*}.toml`, and a translation that mishandles them would
// report a working pattern as dead.
function globToRegExp(pattern) {
  let source = '';
  let braceDepth = 0;
  for (let index = 0; index < pattern.length; index += 1) {
    const character = pattern[index];
    if (character === '*') {
      if (pattern[index + 1] === '*') {
        index += 1;
        if (pattern[index + 1] === '/') {
          index += 1;
          source += '(?:.*/)?';
        } else {
          source += '.*';
        }
      } else {
        source += '[^/]*';
      }
    } else if (character === '?') {
      source += '[^/]';
    } else if (character === '{') {
      braceDepth += 1;
      source += '(?:';
    } else if (character === '}' && braceDepth > 0) {
      braceDepth -= 1;
      source += ')';
    } else if (character === ',' && braceDepth > 0) {
      source += '|';
    } else if ('.+^$()|[]\\}'.includes(character)) {
      source += `\\${character}`;
    } else {
      source += character;
    }
  }
  return new RegExp(`^${source}$`);
}

function selectFiles(pattern) {
  if (pattern.length > 1 && pattern.startsWith('/')) {
    const end = pattern.lastIndexOf('/');
    const regex = new RegExp(pattern.slice(1, end), pattern.slice(end + 1));
    return trackedFiles.filter((path) => regex.test(path));
  }
  const regex = globToRegExp(pattern);
  return trackedFiles.filter((path) => regex.test(path));
}

// ---------------------------------------------------------------------------
// Part 1: every Renovate manager must select real files and extract real pins.
// ---------------------------------------------------------------------------

function checkRenovateManagers() {
  const config = JSON.parse(readFileSync(renovateConfigPath, 'utf8'));
  const rows = [];
  const nativeCoverage = new Map();

  // Native managers whose managerFilePatterns this repository overrides. An
  // override replaces the manager's defaults outright, so a typo silently
  // un-manages every file the defaults used to cover.
  for (const [key, value] of Object.entries(config)) {
    if (
      value === null ||
      typeof value !== 'object' ||
      Array.isArray(value) ||
      !Array.isArray(value.managerFilePatterns)
    ) {
      continue;
    }
    const covered = new Set();
    for (const pattern of value.managerFilePatterns) {
      const hits = selectFiles(pattern);
      for (const hit of hits) covered.add(hit);
      // A default pattern inherited from the manager may legitimately select
      // nothing (this repository has no .rtx.toml). A pattern written out as a
      // literal path is a claim that the file exists, so a typo there is a bug.
      const literal = !/[*?{}[\]]/.test(pattern);
      rows.push([
        `${key} (native)`,
        pattern,
        String(hits.length),
        hits.length > 0
          ? 'ok'
          : literal
            ? 'FAIL: literal path selects no tracked file'
            : 'ok (inherited default, nothing to select)',
      ]);
      if (hits.length === 0 && literal) {
        fail(`native manager "${key}": managerFilePatterns entry ${JSON.stringify(pattern)} names a file that does not exist`);
      }
    }
    if (covered.size === 0) {
      fail(`native manager "${key}": the managerFilePatterns override selects no tracked file at all, so the manager is disabled`);
    }
    nativeCoverage.set(key, covered);
  }

  // An override replaces the manager's defaults outright. The risk is not a
  // pattern that matches nothing, it is a file that no pattern reaches: the
  // manager then silently stops managing it. Every file the manifest declares
  // as belonging to a manager must be selected by that manager.
  const manifest = JSON.parse(readFileSync(pinManifestPath, 'utf8'));
  for (const rule of manifest.managerCoverage ?? []) {
    const regex = compile(`managerCoverage ${rule.manager}`, rule.filesMatching);
    if (!regex) continue;
    const expected = trackedFiles.filter((path) => new RegExp(rule.filesMatching).test(path));
    if (expected.length === 0) {
      rows.push([`${rule.manager} (coverage)`, rule.filesMatching, '0', 'FAIL: rule matches no file']);
      fail(`managerCoverage rule for "${rule.manager}": ${JSON.stringify(rule.filesMatching)} matches no tracked file, so it proves nothing`);
      continue;
    }
    const covered = nativeCoverage.get(rule.manager) ?? new Set();
    for (const file of expected) {
      const isCovered = covered.has(file);
      rows.push([
        `${rule.manager} (coverage)`,
        file,
        isCovered ? '1' : '0',
        isCovered ? 'ok' : `FAIL: unmanaged by ${rule.manager}`,
      ]);
      if (!isCovered) {
        fail(`${file} must be managed by the "${rule.manager}" manager but no managerFilePatterns entry selects it: ${rule.description ?? ''}`.trim());
      }
    }
  }

  const managers = config.customManagers ?? [];
  if (managers.length === 0) {
    fail('renovate.json declares no customManagers');
    return rows;
  }

  managers.forEach((manager, index) => {
    const name = manager.depNameTemplate ?? manager.description ?? `#${index}`;
    const label = `customManagers[${index}] ${name}`;

    if (manager.customType !== 'regex') {
      rows.push([label, '-', '-', `skipped (customType=${manager.customType})`]);
      return;
    }

    const files = [];
    for (const pattern of manager.managerFilePatterns ?? []) {
      const hits = selectFiles(pattern);
      if (hits.length === 0) {
        rows.push([label, pattern, '0', 'FAIL: pattern selects no tracked file']);
        fail(`${label}: managerFilePatterns entry ${JSON.stringify(pattern)} selects no tracked file`);
      }
      for (const hit of hits) if (!files.includes(hit)) files.push(hit);
    }

    const perFile = new Map(files.map((file) => [file, 0]));
    const perMatchString = new Map((manager.matchStrings ?? []).map((s) => [s, 0]));

    for (const file of files) {
      const content = readRepoFile(file);
      for (const matchString of manager.matchStrings ?? []) {
        const regex = compile(`${label}: matchString`, matchString);
        if (!regex) continue;
        for (const match of content.matchAll(regex)) {
          const value = match.groups?.currentValue ?? match.groups?.currentDigest;
          if (!value) {
            fail(`${label}: matchString ${JSON.stringify(matchString)} matched ${file} without capturing currentValue`);
            continue;
          }
          perFile.set(file, perFile.get(file) + 1);
          perMatchString.set(matchString, perMatchString.get(matchString) + 1);
        }
      }
    }

    for (const [file, count] of perFile) {
      rows.push([label, file, String(count), count === 0 ? 'FAIL: no matchString matches this file' : 'ok']);
      if (count === 0) {
        fail(`${label}: target file ${file} is matched by none of its matchStrings`);
      }
    }

    for (const [matchString, count] of perMatchString) {
      if (count === 0) {
        rows.push([label, '(every target)', '0', 'FAIL: dead matchString']);
        fail(
          `${label}: matchString ${JSON.stringify(matchString)} matches nothing in ${
            files.length > 0 ? files.join(', ') : '(no target file)'
          }`,
        );
      }
    }
  });

  return rows;
}

// ---------------------------------------------------------------------------
// Part 2: mise declarations, their lockfiles, and each other.
// ---------------------------------------------------------------------------

// Deliberately small TOML readers. The mise configs and lockfiles in this
// repository are flat tables of string values; a dependency-free reader keeps
// this gate runnable from any environment that has a JavaScript runtime.
function parseMiseTools(content) {
  const tools = new Map();
  let inTools = false;
  for (const rawLine of content.split('\n')) {
    const line = rawLine.trim();
    if (line.startsWith('#') || line === '') continue;
    if (line.startsWith('[')) {
      inTools = line === '[tools]';
      continue;
    }
    if (!inTools) continue;
    const match = /^(?<key>"[^"]+"|[^=\s]+)\s*=\s*"(?<version>[^"]+)"/.exec(line);
    if (match) {
      tools.set(match.groups.key.replace(/^"|"$/g, ''), match.groups.version);
    }
  }
  return tools;
}

function parseMiseLock(content) {
  const versions = new Map();
  let current = null;
  for (const rawLine of content.split('\n')) {
    const line = rawLine.trim();
    if (line.startsWith('[[tools.')) {
      const match = /^\[\[tools\.(?<key>"[^"]+"|[^\]]+)\]\]$/.exec(line);
      current = match ? match.groups.key.replace(/^"|"$/g, '') : null;
      continue;
    }
    if (line.startsWith('[')) {
      current = null;
      continue;
    }
    if (!current) continue;
    const match = /^version\s*=\s*"(?<version>[^"]+)"/.exec(line);
    if (match) {
      versions.set(current, match.groups.version);
      current = null;
    }
  }
  return versions;
}

// "aqua:nextest-rs/nextest/cargo-nextest" and "cargo:cargo-nextest" are the same
// tool pinned through different backends. Compare on the tool, not the backend.
const shortToolName = (name) =>
  name.split('/').pop().replace(/^[a-z0-9-]+:/, '');

function checkMiseCoherence() {
  const rows = [];
  const configs = trackedFiles.filter((path) => /(^|\/)[^/]*mise\.toml$/.test(path));

  if (configs.length === 0) {
    fail('no mise configuration files found; this gate would pass vacuously');
    return rows;
  }

  const declarations = new Map();

  for (const configPath of configs) {
    const tools = parseMiseTools(readRepoFile(configPath));
    if (tools.size === 0) {
      rows.push([configPath, '(no [tools] entries)', '-', 'FAIL: nothing extracted']);
      fail(`${configPath}: no [tools] entries parsed; the gate cannot check a config it cannot read`);
      continue;
    }

    const lockPath = configPath.replace(/\.toml$/, '.lock');
    const hasLock = trackedFiles.includes(lockPath);
    // `mise lock` derives the lockfile from the config path by extension, so a
    // config with `lockfile = true` and no committed lock is an unenforced pin.
    if (!hasLock && /lockfile\s*=\s*true/.test(readRepoFile(configPath))) {
      rows.push([configPath, lockPath, '-', 'FAIL: lockfile = true but no committed lock']);
      fail(`${configPath}: declares lockfile = true but ${lockPath} is not committed`);
    }
    const locked = hasLock ? parseMiseLock(readRepoFile(lockPath)) : new Map();

    for (const [key, version] of tools) {
      const short = shortToolName(key);
      if (!declarations.has(short)) declarations.set(short, []);
      declarations.get(short).push({ configPath, key, version });

      if (!hasLock) continue;
      const lockedVersion = locked.get(key) ?? locked.get(short);
      if (lockedVersion === undefined) {
        rows.push([configPath, lockPath, `${key} ${version}`, 'FAIL: absent from lockfile']);
        fail(
          `${configPath}: ${key} is pinned to ${version} but has no entry in ${lockPath}; mise resolves tools from the lock, so the declared version would not be installed`,
        );
        continue;
      }
      const agrees = lockedVersion === version;
      rows.push([
        configPath,
        lockPath,
        `${key} ${version}`,
        agrees ? 'ok' : `FAIL: lock holds ${lockedVersion}`,
      ]);
      if (!agrees) {
        fail(
          `${configPath}: ${key} is declared ${version} but ${lockPath} still locks ${lockedVersion}; regenerate with \`mise lock ${short}\``,
        );
      }
    }
  }

  for (const [short, sites] of declarations) {
    if (sites.length < 2) continue;
    const versions = [...new Set(sites.map((site) => site.version))];
    if (versions.length === 1) {
      rows.push([
        `${short} (${sites.length} configs)`,
        sites.map((site) => site.configPath).join(', '),
        versions[0],
        'ok',
      ]);
      continue;
    }
    for (const site of sites) {
      rows.push([`${short} (cross-config)`, site.configPath, site.version, 'FAIL: configs disagree']);
    }
    fail(
      `${short} is pinned to different versions across mise configs: ${sites
        .map((site) => `${site.configPath}=${site.version}`)
        .join(', ')}. The local, build and job environments must run the same tool.`,
    );
  }

  return rows;
}

// ---------------------------------------------------------------------------
// Part 3: version literals repeated outside mise, declared in the manifest.
// ---------------------------------------------------------------------------

function extract(label, file, pattern) {
  const regex = compile(label, pattern);
  if (!regex) return [];
  let content;
  try {
    content = readRepoFile(file);
  } catch (error) {
    fail(`${label}: cannot read ${file}: ${error.message}`);
    return [];
  }
  const values = [];
  for (const match of content.matchAll(regex)) {
    const value = match.groups?.version;
    if (!value) {
      fail(`${label}: pattern matched ${file} without capturing a "version" group`);
      continue;
    }
    values.push(value);
  }
  return values;
}

function checkDeclaredPins() {
  const manifest = JSON.parse(readFileSync(pinManifestPath, 'utf8'));
  const rows = [];
  const pins = manifest.pins ?? [];

  if (pins.length === 0) {
    fail('config/version-pins.json declares no pins; this gate would pass vacuously');
    return rows;
  }

  for (const pin of pins) {
    const authorityLabel = `pin ${pin.name}: authority ${pin.authority.file}`;
    const authorityValues = extract(authorityLabel, pin.authority.file, pin.authority.pattern);

    if (authorityValues.length === 0) {
      rows.push([pin.name, `${pin.authority.file} (authority)`, '-', 'FAIL: authoritative pin not found']);
      fail(`${authorityLabel}: the authoritative pin site matched nothing, so nothing can be compared against it`);
      continue;
    }

    const unique = [...new Set(authorityValues)];
    if (unique.length > 1) {
      rows.push([pin.name, `${pin.authority.file} (authority)`, unique.join(' / '), 'FAIL: authority disagrees with itself']);
      fail(`${authorityLabel}: the authoritative site declares more than one version: ${unique.join(', ')}`);
      continue;
    }

    const expected = unique[0];
    rows.push([pin.name, `${pin.authority.file} (authority)`, expected, 'authority']);

    for (const mirror of pin.mirrors ?? []) {
      const mirrorLabel = `pin ${pin.name}: copy in ${mirror.file}`;
      const values = extract(mirrorLabel, mirror.file, mirror.pattern);
      if (values.length === 0) {
        rows.push([pin.name, mirror.file, '-', 'FAIL: copy not found']);
        fail(
          `${mirrorLabel}: expected a copy of the ${pin.name} pin here, but the pattern matched nothing. Either the pin site moved or the manifest is stale; both leave the copy unchecked.`,
        );
        continue;
      }
      for (const value of new Set(values)) {
        const agrees = value === expected;
        rows.push([pin.name, mirror.file, value, agrees ? 'ok' : `FAIL: expected ${expected}`]);
        if (!agrees) {
          fail(`${mirrorLabel}: holds ${value} but the authoritative site ${pin.authority.file} declares ${expected}`);
        }
      }
    }
  }

  return rows;
}

section('Renovate manager coverage', ['manager', 'target file', 'matches', 'verdict'], checkRenovateManagers());
section('mise declarations against their lockfiles and each other', ['config', 'compared with', 'pin', 'verdict'], checkMiseCoherence());
section('Version literals repeated outside mise', ['pin', 'site', 'version', 'verdict'], checkDeclaredPins());

if (failures.length > 0) {
  console.error(`pin-integrity: ${failures.length} failure(s):`);
  for (const failure of failures) console.error(`  - ${failure}`);
  console.error('');
  console.error(
    'A custom manager that matches nothing opens no update pull request, and a pin repeated without comparison drifts. Point the manager at the real pin site, or make the copies agree.',
  );
  process.exit(1);
}

console.log('pin-integrity: every Renovate manager extracts a real pin, and every repeated pin agrees with its authority');
