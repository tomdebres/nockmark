#!/usr/bin/env node
// publish-check: is this fit to be public? From devkit's
// templates/publish-check.mjs (kit-install --public puts it in scripts/):
// change it there, not here.
//
//   publish-check [--range <a>..<b> | --tree-only] [--github <owner/name>]
//                 [--patterns <file> | --no-personal] [--secrets-env <file>]
//                 [--summary] <repo-or-dir>
//
// <repo-or-dir> is a checkout (its working tree, the history of every ref and
// every commit's metadata), a bare repo (HEAD's tree stands for the working
// tree), a plain directory (its files only), or owner/name on GitHub, which is
// mirror-cloned to a temp dir (every ref GitHub keeps, pull requests' heads
// included) and implies --github. --range checks only those commits (CI, a
// pre-push hook); --tree-only only the files.
//
// What it looks for:
//   agent-file        agent working docs (CLAUDE.md, AGENTS.md, HANDOVER.md,
//                     DECISIONS.md, REVIEW.md, PLAN.md, .claude/, .orca, session
//                     notes, …) at any depth, in the tree or ever in history
//   personal:<label>  a value from the private patterns file: in a file, a
//                     commit message, an author or committer name, or (with
//                     --github) an issue, pull request, comment or review
//   email             an author or committer address that isn't GitHub's noreply
//   secret:<rule>     gitleaks' rules when it's installed, else built-in ones;
//                     and any value from --secrets-env (labelled by its key)
//   workflow          a workflow on a self-hosted runner: a fork's PR runs there
// Each finding is one line, its kind then where it is (file:line, the commit,
// and so on), never the value. A history finding is one the tree no longer has.
//
// The patterns file is private and lives outside every repo: --patterns, else
// $PUBLISH_PATTERNS. Each line is "<label> <value>": a literal (matched as a
// whole word; ignoring case when it's all lower case), or re:<regex>, or
// rei:<regex> to ignore case. A "public <value>" line marks a value as public
// (a project's own domain, say): the builder leaves it out, and the check skips
// it. Lines starting # are comments. Without the file, the check refuses unless
// --no-personal.
// --secrets-env (else $PUBLISH_SECRETS_ENV) is a KEY=value file whose values
// of 8 characters or more are looked for exactly.
//
// Exit 0 clean, 1 findings, 2 usage or it can't check.
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, lstatSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const MAX_BLOB = 2 * 1024 * 1024; // larger files are data, not prose
const GITLEAKS = process.env.PUBLISH_CHECK_GITLEAKS || 'gitleaks'; // a path that isn't there: the built-in rules
const BUF = 1024 * 1024 * 1024;

// ---- the rules -------------------------------------------------------------

// Agent working docs, by name at any depth (case ignored), by directory, or by
// path prefix. A public repo carries none of them (devkit PLAN.md › Public repos).
const AGENT_NAMES = new Set(['claude.md', 'claude.local.md', 'agents.md', 'agent.md', 'gemini.md', 'handover.md', 'handoff.md',
  'decisions.md', 'review.md', 'plan.md', 'memory.md', 'local.md', 'copilot-instructions.md', '.cursorrules',
  '.windsurfrules', '.clinerules', '.mcp.json', '.aider.chat.history.md', '.aider.input.history', '.aider.conf.yml']);
const AGENT_DIRS = new Set(['.claude', '.orca', '.cursor', '.codex', '.gemini', '.agents', '.superpowers', '.specstory', '.remember']);
const AGENT_PREFIXES = ['docs/superpowers'];
const AGENT_NOTES = /^(session|sessions|handover|handoff)[-_ .].*\.md$|[-_]brief\.md$/i;

export function isAgentPath(path) {
  const parts = path.split('/');
  const base = parts.at(-1);
  return AGENT_NAMES.has(base.toLowerCase()) || AGENT_NOTES.test(base) ||
    parts.some((p) => AGENT_DIRS.has(p.toLowerCase())) ||
    AGENT_PREFIXES.some((p) => path.toLowerCase() === p || path.toLowerCase().startsWith(`${p}/`));
}

// GitHub's own addresses: an account's noreply, the web-flow committer, Actions.
export const isNoreply = (email) =>
  /@users\.noreply\.github\.com$/i.test(email) || /^(noreply|action)@github\.com$/i.test(email);

// Built from parts so this file never matches itself.
const B = String.raw;
export const SECRET_RULES = [
  ['private-key', new RegExp('-----BEGIN [A-Z ]*' + 'PRIVATE KEY-----')],
  ['github-token', new RegExp(B`\b(gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{60,})\b`)],
  ['anthropic-key', new RegExp(B`\bsk-ant-[A-Za-z0-9_-]{20,}`)],
  ['openai-key', new RegExp(B`\bsk-(proj-)?[A-Za-z0-9]{20}T3BlbkFJ[A-Za-z0-9]{20}`)],
  ['aws-access-key', new RegExp(B`\b(AKIA|ASIA)[0-9A-Z]{16}\b`)],
  ['slack-token', new RegExp(B`\bxox[abprs]-[A-Za-z0-9-]{10,}`)],
  ['telegram-bot-token', new RegExp(B`\b\d{8,10}:AA[A-Za-z0-9_-]{33}\b`)],
  ['google-api-key', new RegExp(B`\bAIza[0-9A-Za-z_-]{35}\b`)],
  ['stripe-key', new RegExp(B`\b[rs]k_live_[0-9A-Za-z]{20,}`)],
  ['jwt', new RegExp(B`\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}`)],
  ['assigned-secret', new RegExp(B`(password|passwd|secret|api[_-]?key|token)["']?\s*[:=]\s*["'][^"'\s]{12,}["']`, 'i')],
].map(([label, re]) => ({ kind: 'secret', label, re }));

// A literal matches as a whole word at its word-character ends; all lower case,
// it ignores case (so a domain or path matches however it's written).
export function literal(v) {
  const esc = v.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');
  return new RegExp(`${/^\w/.test(v) ? '\\b' : ''}${esc}${/\w$/.test(v) ? '\\b' : ''}`, v === v.toLowerCase() ? 'i' : '');
}

// The private patterns file. Errors name the line and label, never the value.
export function parsePatterns(text) {
  const rules = [];
  text.split('\n').forEach((raw, i) => {
    const line = raw.replace(/\r$/, '');
    if (!line.trim() || line.trimStart().startsWith('#')) return;
    const m = line.match(/^\s*([A-Za-z0-9][A-Za-z0-9._:@-]*)\s+(\S.*?)\s*$/);
    if (!m) throw new Error(`patterns line ${i + 1} isn't "<label> <value>"`);
    const [, label, value] = m;
    if (label === 'public') return;
    const rx = value.match(/^re(i?):(.+)$/);
    let re;
    try {
      re = rx ? new RegExp(rx[2], rx[1]) : literal(value);
    } catch {
      throw new Error(`patterns line ${i + 1} (${label}) isn't a valid regular expression`);
    }
    if (re.test('')) throw new Error(`patterns line ${i + 1} (${label}) matches everything`);
    rules.push({ kind: 'personal', label, re });
  });
  return rules;
}

// Exact values from a KEY=value file, labelled by key; a comma list counts
// each item. Shorter than 8 characters is too common a string to mean anything.
export function parseSecretsEnv(text) {
  const rules = [];
  const seen = new Set();
  for (const raw of text.split('\n')) {
    const m = raw.trim().replace(/^export\s+/, '').match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
    if (!m) continue;
    let v = m[2].trim();
    if (/^(['"]).*\1$/.test(v)) v = v.slice(1, -1);
    for (const part of [v, ...(v.includes(',') ? v.split(',').map((s) => s.trim()) : [])]) {
      if (part.length < 8 || seen.has(part)) continue;
      seen.add(part);
      rules.push({ kind: 'secret', label: `secrets-env:${m[1]}`, re: new RegExp(part.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')) });
    }
  }
  return rules;
}

// Which rules a text breaks, and on which lines: [{ rule, line, text }], once
// per rule and line. A match may span lines (a wrapped address): it counts on
// the line it starts on.
export function scanText(text, rules) {
  const hit = rules.filter((r) => r.re.test(text));
  if (!hit.length) return [];
  const starts = [0];
  for (let i = text.indexOf('\n'); i >= 0; i = text.indexOf('\n', i + 1)) starts.push(i + 1);
  const lineAt = (at) => { let lo = 0, hi = starts.length - 1; while (lo < hi) { const m = (lo + hi + 1) >> 1; if (starts[m] <= at) lo = m; else hi = m - 1; } return lo; };
  const out = [];
  for (const rule of hit) {
    const g = new RegExp(rule.re.source, rule.re.flags.replace(/[gy]/g, '') + 'g');
    let last = -1;
    for (let m = g.exec(text); m; m = g.exec(text)) {
      if (m[0] === '') g.lastIndex++;
      const n = lineAt(m.index);
      if (n === last) continue;
      last = n;
      out.push({ rule, line: n + 1, text: text.slice(starts[n], (starts[n + 1] ?? text.length + 1) - 1) });
    }
  }
  return out.sort((a, b) => a.line - b.line);
}

const isBinary = (buf) => buf.subarray(0, 8000).includes(0);
const hash = (s) => { let h = 5381; for (let i = 0; i < s.length; i++) h = ((h * 33) ^ s.charCodeAt(i)) >>> 0; return h.toString(36); };

// ---- git -------------------------------------------------------------------

function gitRun(dir, args, opts = {}) {
  const r = spawnSync('git', ['-C', dir, '-c', 'core.quotepath=off', ...args], { maxBuffer: BUF, ...opts });
  if (r.status !== 0) throw new Error(`git ${args[0]}: ${String(r.stderr || r.error?.message || '').trim().split('\n')[0]}`);
  return r.stdout;
}
const gitText = (dir, args) => gitRun(dir, args, { encoding: 'utf8' });

// The objects' contents, by id: cat-file --batch over chunks, so nothing holds
// more than ~64 MB of output at once.
function* blobs(dir, ids) {
  let chunk = [];
  let size = 0;
  const flush = function* () {
    const out = gitRun(dir, ['cat-file', '--batch'], { input: chunk.map((c) => c.id).join('\n') + '\n' });
    let at = 0;
    for (const c of chunk) {
      const nl = out.indexOf(10, at);
      const [id, type, len] = out.subarray(at, nl).toString().split(' ');
      const n = Number(len);
      if (type !== 'blob') { at = nl + 1 + (Number.isFinite(n) ? n + 1 : 0); continue; }
      yield [id, out.subarray(nl + 1, nl + 1 + n)];
      at = nl + 1 + n + 1;
    }
    chunk = [];
    size = 0;
  };
  for (const b of ids) {
    chunk.push(b);
    size += b.size;
    if (size > 64 * 1024 * 1024) yield* flush();
  }
  if (chunk.length) yield* flush();
}

// ---- the check -------------------------------------------------------------

function usage(why) {
  if (why) console.error(`publish-check: ${why}`);
  console.error('usage: publish-check [--range <a>..<b> | --tree-only] [--github <owner/name>] [--patterns <file> | --no-personal] [--secrets-env <file>] [--summary] <repo-or-dir>');
  process.exit(2);
}

function parseArgs(argv) {
  const o = { range: null, treeOnly: false, github: null, patterns: process.env.PUBLISH_PATTERNS || null, personal: true,
    secretsEnv: process.env.PUBLISH_SECRETS_ENV || null, summary: false, target: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => { const v = argv[++i]; if (v === undefined || v.startsWith('--')) usage(`${a} needs a value`); return v; };
    if (a === '--range') o.range = val();
    else if (a === '--tree-only') o.treeOnly = true;
    else if (a === '--github') o.github = val();
    else if (a === '--patterns') o.patterns = val();
    else if (a === '--no-personal') o.personal = false;
    else if (a === '--secrets-env') o.secretsEnv = val();
    else if (a === '--summary') o.summary = true;
    else if (a.startsWith('-')) usage(`unknown option ${a}`);
    else if (o.target) usage('one repo or directory');
    else o.target = a;
  }
  if (!o.target) usage();
  if (o.range && o.treeOnly) usage('--range or --tree-only, not both');
  if (o.range && !/^[^\s.][^\s]*\.\.[^\s.][^\s]*$/.test(o.range)) usage('--range is <a>..<b>');
  if (o.github && !/^[\w.-]+\/[\w.-]+$/.test(o.github)) usage('--github is owner/name');
  return o;
}

export function check(o) {
  const findings = [];
  const seen = new Set();
  const add = (kind, where, key = `${kind}|${where}`) => {
    if (!seen.has(key)) { seen.add(key); findings.push({ kind, where }); }
  };
  const notes = [];

  let rules = [];
  if (o.personal) {
    if (!o.patterns) throw Object.assign(new Error('no patterns file: pass --patterns <file> (or set PUBLISH_PATTERNS), or --no-personal'), { code: 2 });
    if (!existsSync(o.patterns)) throw Object.assign(new Error(`no patterns file at ${o.patterns}: build it, or pass --no-personal`), { code: 2 });
    rules = parsePatterns(readFileSync(o.patterns, 'utf8'));
    if (!rules.length) throw Object.assign(new Error(`${o.patterns} has no patterns`), { code: 2 });
  }
  const personal = rules;
  if (o.secretsEnv) {
    if (existsSync(o.secretsEnv)) rules = rules.concat(parseSecretsEnv(readFileSync(o.secretsEnv, 'utf8')));
    else notes.push(`no secrets file at ${o.secretsEnv}: its values weren't looked for`);
  }

  // The target: a GitHub repo is cloned with every ref it keeps.
  let dir = o.target;
  let temp = null;
  let github = o.github;
  if (!existsSync(dir) && /^[\w.-]+\/[\w.-]+$/.test(dir)) {
    temp = mkdtempSync(join(tmpdir(), 'publish-check-'));
    const r = spawnSync('git', ['clone', '--quiet', '--mirror', `https://github.com/${dir}.git`, join(temp, 'repo.git')], { encoding: 'utf8' });
    if (r.status !== 0) { rmSync(temp, { recursive: true, force: true }); throw Object.assign(new Error(`can't clone ${dir}: ${r.stderr.trim().split('\n')[0]}`), { code: 2 }); }
    github ??= dir;
    dir = join(temp, 'repo.git');
  }
  if (!existsSync(dir) || !statSync(dir).isDirectory()) throw Object.assign(new Error(`${o.target} isn't a directory or owner/name`), { code: 2 });
  dir = realpathSync(dir);

  try {
    const inGit = spawnSync('git', ['-C', dir, 'rev-parse', '--is-bare-repository'], { encoding: 'utf8' });
    const isGit = inGit.status === 0 && (inGit.stdout.trim() === 'true' ||
      realpathSync(gitText(dir, ['rev-parse', '--show-toplevel']).trim()) === dir);
    const bare = isGit && inGit.stdout.trim() === 'true';
    const hasHead = isGit && spawnSync('git', ['-C', dir, 'rev-parse', '--verify', '--quiet', 'HEAD^{commit}']).status === 0;
    // Refs with commits, whatever HEAD is: an orphan branch checked out, or a
    // bare repo whose HEAD names a missing branch, still has every other ref.
    const hasRefs = isGit && (hasHead || gitText(dir, ['for-each-ref', '--count=1', '--format=x', '--exclude=refs/stash']).trim() !== '');
    if (o.range && !isGit) throw Object.assign(new Error('--range needs a git repo'), { code: 2 });

    // gitleaks for secrets when it's there; built-in rules otherwise, and for a
    // bare repo's tree alone, which gitleaks has no way to read.
    const gl = spawnSync(GITLEAKS, ['version'], { encoding: 'utf8' });
    const gitleaks = gl.status === 0 && !(bare && o.treeOnly);
    if (!gitleaks) rules = rules.concat(SECRET_RULES);
    const contentRules = rules;
    const treeKeys = new Set(); // personal or secret hits the tree has: history reports only the rest

    // ---- the tree
    const files = []; // [path, () => Buffer]
    if (!isGit) {
      const walk = (rel) => {
        for (const e of readdirSync(join(dir, rel), { withFileTypes: true })) {
          const p = rel ? `${rel}/${e.name}` : e.name;
          if (e.name === '.git' && !rel) continue;
          if (e.isDirectory()) walk(p);
          else if (e.isFile()) files.push([p, () => readFileSync(join(dir, p)), statSync(join(dir, p)).size]);
          else if (e.isSymbolicLink()) files.push([p, null, 0]);
        }
      };
      walk('');
    } else if (!bare) {
      const list = gitText(dir, ['ls-files', '-z', '--cached', '--others', '--exclude-standard']).split('\0').filter(Boolean);
      for (const p of [...new Set(list)]) {
        const st = lstatSync(join(dir, p), { throwIfNoEntry: false });
        if (!st) continue; // deleted in the tree, not yet committed
        files.push([p, st.isFile() ? () => readFileSync(join(dir, p)) : null, st.isFile() ? st.size : 0]);
      }
    } else if (hasHead) {
      const ls = gitText(dir, ['ls-tree', '-r', '-z', '--long', 'HEAD']).split('\0').filter(Boolean);
      const ids = [];
      for (const row of ls) {
        const [meta, p] = row.split('\t');
        const [, type, id, size] = meta.trim().split(/\s+/);
        if (type !== 'blob') continue;
        files.push([p, id, Number(size)]);
        if (Number(size) <= MAX_BLOB) ids.push({ id, size: Number(size) });
      }
      const content = new Map(blobs(dir, ids));
      for (const f of files) { const id = f[1]; f[1] = content.has(id) ? () => content.get(id) : null; }
    }
    for (const [p, read, size] of files) {
      if (isAgentPath(p)) add('agent-file', p);
      if (!read || size > MAX_BLOB) continue;
      const buf = read();
      if (isBinary(buf)) continue;
      const text = buf.toString('utf8');
      for (const h of scanText(text, contentRules)) {
        treeKeys.add(`${h.rule.label}|${p}|${hash(h.text)}`);
        add(`${h.rule.kind}:${h.rule.label}`, `${p}:${h.line}`);
      }
      if (/^\.github\/workflows\/[^/]+\.ya?ml$/.test(p)) {
        text.split('\n').forEach((t, i) => {
          if (/^\s*#/.test(t)) return;
          if (/\bself-hosted\b/.test(t)) add('workflow', `${p}:${i + 1} runs on a self-hosted runner`);
        });
      }
    }
    // gitleaks reads the files of a plain directory, or a checkout's when only
    // the tree is asked for (ignored files never leave the machine). Otherwise
    // its history scan below covers what's committed, and a finding whose
    // secret the tree still has counts as a tree finding.
    const readers = new Map(files.map(([p, read]) => [p, read]));
    if (gitleaks && (!isGit || (o.treeOnly && !bare))) {
      for (const f of runGitleaks(['dir', dir])) {
        const file = f.File.startsWith(`${dir}/`) ? f.File.slice(dir.length + 1) : f.File;
        if (isGit && !readers.has(file)) continue;
        add(`secret:${f.RuleID}`, `${file}:${f.StartLine}`);
      }
    }

    // ---- history and metadata
    let commits = 0;
    if (isGit && !o.treeOnly && hasRefs) {
      const revs = o.range ? [o.range] : ['--exclude=refs/stash', '--all'];
      commits = Number(gitText(dir, ['rev-list', '--count', ...revs]).trim());

      // Agent files ever added, with the first commit that added each.
      const added = new Map();
      let at = '';
      for (const row of gitText(dir, ['log', '--reverse', '--format=@%h', '--abbrev=7', '--name-only', '--diff-filter=A', '--no-renames', ...revs]).split('\n')) {
        if (row.startsWith('@')) at = row.slice(1, 8);
        else if (row && !added.has(row)) added.set(row, at);
      }
      const inTree = new Set(files.map(([p]) => p));
      for (const [p, sha] of added) if (isAgentPath(p) && !inTree.has(p)) add('agent-file', `history ${p} (added in ${sha})`);

      // Every blob the commits reach, read once; a hit names the first commit
      // that introduced the blob.
      const objs = gitText(dir, ['rev-list', '--objects', ...revs]).split('\n').filter(Boolean);
      const pathOf = new Map();
      for (const row of objs) { const i = row.indexOf(' '); if (i > 0) pathOf.set(row.slice(0, i), row.slice(i + 1)); }
      const checkIn = [...pathOf.keys()].join('\n') + '\n';
      const sizes = gitRun(dir, ['cat-file', '--batch-check=%(objectname) %(objecttype) %(objectsize)'], { input: checkIn, encoding: 'utf8' });
      const want = [];
      for (const row of sizes.split('\n')) {
        const [id, type, size] = row.split(' ');
        if (type === 'blob' && Number(size) <= MAX_BLOB) want.push({ id, size: Number(size) });
      }
      const firstIn = new Map();
      for (const row of gitText(dir, ['log', '--reverse', '--format=@%h', '--raw', '--no-abbrev', '--no-renames', '--diff-merges=first-parent', ...revs]).split('\n')) {
        if (row.startsWith('@')) { at = row.slice(1, 8); continue; }
        const m = row.match(/^:\d+ \d+ [0-9a-f]+ ([0-9a-f]+) [A-Z]/);
        if (m && !firstIn.has(m[1])) firstIn.set(m[1], at);
      }
      for (const [id, buf] of blobs(dir, want)) {
        if (isBinary(buf)) continue;
        const p = pathOf.get(id);
        for (const h of scanText(buf.toString('utf8'), contentRules)) {
          const key = `${h.rule.label}|${p}|${hash(h.text)}`;
          if (treeKeys.has(key)) continue;
          add(`${h.rule.kind}:${h.rule.label}`, `history ${p}:${h.line} (in ${firstIn.get(id) ?? 'a merge'})`, key);
        }
      }

      // Messages, names and addresses. Each address that isn't noreply gets a
      // number, so its commits can be counted without printing it.
      const addrs = new Map();
      const log = gitText(dir, ['log', '--abbrev=7', '--format=%h%x00%an%x00%ae%x00%cn%x00%ce%x00%B%x1e', ...revs]);
      for (const rec of log.split('\x1e')) {
        const [sha, an, ae, cn, ce, body] = rec.replace(/^\n/, '').split('\0');
        if (!sha) continue;
        for (const [role, name, email] of [['author', an, ae], ['committer', cn, ce]]) {
          for (const h of scanText(name, personal)) add(`personal:${h.rule.label}`, `${sha} ${role} name`);
          if (isNoreply(email)) continue;
          if (!addrs.has(email.toLowerCase())) addrs.set(email.toLowerCase(), addrs.size + 1);
          const label = scanText(email, personal)[0]?.rule.label;
          add('email', `${sha} ${role}: address ${addrs.get(email.toLowerCase())}${label ? ` (${label})` : ''}, not GitHub's noreply`);
        }
        for (const h of scanText(body ?? '', contentRules)) add(`${h.rule.kind}:${h.rule.label}`, `${sha} message:${h.line}`);
      }

      // Annotated tags: every one, or with --range the tag a push names as its
      // right side. Their tagger and message are as public as a commit's.
      const tagRows = o.range
        ? (gitText(dir, ['cat-file', '-t', o.range.split('..')[1]]).trim() === 'tag' ? [o.range.split('..')[1]] : [])
        : gitText(dir, ['for-each-ref', '--format=%(objectname) %(objecttype)', 'refs/tags']).split('\n').filter((l) => l.endsWith(' tag')).map((l) => l.split(' ')[0]);
      for (const id of tagRows) {
        const raw = gitText(dir, ['cat-file', 'tag', id]);
        const head = raw.slice(0, raw.indexOf('\n\n') >= 0 ? raw.indexOf('\n\n') : raw.length);
        const msg = raw.indexOf('\n\n') >= 0 ? raw.slice(raw.indexOf('\n\n') + 2) : '';
        const name = head.match(/^tag (.*)$/m)?.[1] ?? id.slice(0, 7);
        const tagger = head.match(/^tagger (.*?) <([^>]*)>/m);
        if (tagger) {
          for (const h of scanText(tagger[1], personal)) add(`personal:${h.rule.label}`, `tag ${name} tagger name`);
          if (!isNoreply(tagger[2])) {
            if (!addrs.has(tagger[2].toLowerCase())) addrs.set(tagger[2].toLowerCase(), addrs.size + 1);
            const label = scanText(tagger[2], personal)[0]?.rule.label;
            add('email', `tag ${name} tagger: address ${addrs.get(tagger[2].toLowerCase())}${label ? ` (${label})` : ''}, not GitHub's noreply`);
          }
        }
        for (const h of scanText(msg, contentRules)) add(`${h.rule.kind}:${h.rule.label}`, `tag ${name} message:${h.line}`);
      }

      if (gitleaks) {
        for (const f of runGitleaks(['git', dir, `--log-opts=${revs.join(' ')}`])) {
          const read = readers.get(f.File);
          const now = read && f.Secret ? read().toString('utf8').split('\n').findIndex((l) => l.includes(f.Secret)) : -1;
          if (now >= 0) add(`secret:${f.RuleID}`, `${f.File}:${now + 1}`);
          else add(`secret:${f.RuleID}`, `history ${f.File}:${f.StartLine} (in ${String(f.Commit).slice(0, 7)})`, `secret|${f.RuleID}|${f.File}|${hash(f.Secret ?? '')}`);
        }
      }
    }

    // ---- GitHub: issues, pull requests, comments, reviews, releases
    let gh = 0;
    if (github) gh = scanGitHub(github, contentRules, add);

    return { findings, notes, commits, files: files.length, gitleaks, github: gh };
  } finally {
    if (temp) rmSync(temp, { recursive: true, force: true });
  }
}

// gitleaks' findings. Its report holds the raw secret, in a private temp dir
// removed straight after: the value is only compared and hashed, never printed.
function runGitleaks(args) {
  const tmp = mkdtempSync(join(tmpdir(), 'publish-check-gl-'));
  try {
    const report = join(tmp, 'report.json');
    const r = spawnSync(GITLEAKS, [...args, '--no-banner', '--log-level', 'error', '--exit-code', '0',
      '--report-format', 'json', '--report-path', report], { encoding: 'utf8', maxBuffer: BUF });
    if (r.status !== 0 || !existsSync(report)) throw Object.assign(new Error(`gitleaks ${args[0]} failed: ${String(r.stderr).trim().split('\n')[0]}`), { code: 2 });
    return JSON.parse(readFileSync(report, 'utf8') || '[]');
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

function ghJSON(path) {
  const r = spawnSync('gh', ['api', '--paginate', '--slurp', path], { encoding: 'utf8', maxBuffer: BUF });
  if (r.status !== 0) throw Object.assign(new Error(`gh api ${path.split('?')[0]}: ${String(r.stderr).trim().split('\n')[0]}`), { code: 2 });
  return JSON.parse(r.stdout).flat();
}

function scanGitHub(repo, rules, add) {
  let n = 0;
  const scan = (text, where) => { n++; for (const h of scanText(text ?? '', rules)) add(`${h.rule.kind}:${h.rule.label}`, `github ${where}:${h.line}`); };
  const meta = ghJSON(`repos/${repo}`)[0] ?? {};
  scan([meta.description, meta.homepage].filter(Boolean).join('\n'), 'description');
  const issues = ghJSON(`repos/${repo}/issues?state=all&per_page=100`);
  for (const i of issues) {
    const kind = i.pull_request ? 'pr' : 'issue';
    scan(i.title, `${kind} #${i.number} title`);
    scan(i.body, `${kind} #${i.number} body`);
    if (i.pull_request) for (const r of ghJSON(`repos/${repo}/pulls/${i.number}/reviews?per_page=100`)) scan(r.body, `pr #${i.number} review ${r.id}`);
  }
  for (const c of ghJSON(`repos/${repo}/issues/comments?per_page=100`)) scan(c.body, `comment ${c.id} on #${c.issue_url?.split('/').pop()}`);
  for (const c of ghJSON(`repos/${repo}/pulls/comments?per_page=100`)) scan(c.body, `review comment ${c.id} on #${c.pull_request_url?.split('/').pop()}`);
  for (const r of ghJSON(`repos/${repo}/releases?per_page=100`)) { scan(r.name, `release ${r.tag_name} name`); scan(r.body, `release ${r.tag_name} body`); }
  return n;
}

function main(argv) {
  const o = parseArgs(argv);
  let r;
  try {
    r = check(o);
  } catch (e) {
    console.error(`publish-check: ${e.message}`);
    return e.code ?? 2;
  }
  const counts = {};
  for (const f of r.findings) counts[f.kind.split(':')[0]] = (counts[f.kind.split(':')[0]] ?? 0) + 1;
  if (!o.summary) for (const f of r.findings) console.log(`${f.kind}\t${f.where}`);
  for (const n of r.notes) console.error(`publish-check: note: ${n}`);
  const scope = [`${r.files} files`, o.treeOnly ? null : `${r.commits} commits`, r.github ? `${r.github} GitHub texts` : null,
    r.gitleaks ? 'secrets by gitleaks' : 'secrets by built-in rules', o.personal ? 'personal patterns' : 'no personal patterns'].filter(Boolean).join(', ');
  if (!r.findings.length) { console.log(`publish-check: clean (${scope})`); return 0; }
  const byKind = Object.entries(counts).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => `${k} ${v}`).join(', ');
  console.log(`publish-check: ${r.findings.length} findings: ${byKind} (${scope})`);
  return 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) process.exit(main(process.argv.slice(2)));
