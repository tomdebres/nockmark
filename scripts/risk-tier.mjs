#!/usr/bin/env node
// Risk tier: which high-risk paths (REVIEW.md › High-risk paths) a change
// touches, and whether Tom has approved it. From devkit's
// templates/risk-tier.mjs (kit-install --risk-tier): change it there, not here.
//
//   node scripts/risk-tier.mjs match < name-status    # `git diff --name-status -M` output
//   node scripts/risk-tier.mjs check --repo <owner/name> --pr <n>
//   node scripts/risk-tier.mjs rerun --repo <owner/name> --pr <n>
//   node scripts/risk-tier.mjs codeowners             # what .github/CODEOWNERS must say
//
// `match` prints one line per matched path, "<path>\t<why>", and nothing for an
// ordinary change, for a land script to tell Tom before he's asked. `check` is
// the "Risk tier" job: it fails when the PR touches a high-risk path and Tom
// hasn't approved its head commit with a review (GitHub requests it from
// CODEOWNERS). The workflow runs it from main's checkout, so the table and this
// file are main's, never the PR's. `rerun` is what a review does
// (risk-tier-review.yml): it reruns the PR's newest Risk tier run when the
// review has changed the verdict.
//
// The table is REVIEW.md's, or in a public repo, which keeps no REVIEW.md,
// .github/high-risk-paths.md's (devkit PLAN.md › Public repos). On a public
// repo every pull request from anyone but Tom and the bot, or from a fork,
// needs Tom's approval of its head too, whatever it touches: an outside PR
// never merges on green alone.
import { existsSync, readFileSync, appendFileSync, realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const NEEDS_TOM = 'needs-tom';
export const APPROVER = 'tomdebres';
export const TRUSTED = [APPROVER, 'tomdebres-agent'];
// A public repo's own table first: its REVIEW.md, if any, is a link into the
// private notes, which CI never sees.
export const TABLES = ['.github/high-risk-paths.md', 'REVIEW.md'];
const START = '<!-- high-risk-paths -->';
const END = '<!-- /high-risk-paths -->';

// Root-relative globs: `*` and `?` stay inside one directory, `**` crosses any
// number, and `**/` may be none at all.
export function globToRegExp(glob) {
  if (!glob || glob.startsWith('/') || glob.includes('\\')) throw new Error(`not a root-relative glob: ${glob}`);
  let re = '';
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === '*' && glob[i + 1] === '*') {
      if (glob[i + 2] === '/') { re += '(?:.*/)?'; i += 2; } else { re += '.*'; i += 1; }
    } else if (c === '*') re += '[^/]*';
    else if (c === '?') re += '[^/]';
    else re += c.replace(/[.+^${}()|[\]]/g, '\\$&');
  }
  return new RegExp(`^${re}$`);
}

// The table between the markers: | `glob` | any or deleted | why |.
export function parseRules(markdown) {
  const start = markdown.indexOf(START), end = markdown.indexOf(END);
  if (start < 0 || end < start) throw new Error(`no ${START} … ${END} block`);
  const rules = [];
  for (const row of markdown.slice(start + START.length, end).split('\n').map((l) => l.trim())) {
    if (!row) continue;
    const cells = row.replace(/^\||\|$/g, '').split('|').map((c) => c.trim());
    if (!row.startsWith('|') || cells.length !== 3) throw new Error(`not a three-column row: ${row}`);
    if (cells[0] === 'Path' || /^-+$/.test(cells[0])) continue;
    const glob = cells[0].replace(/^`(.*)`$/, '$1');
    if (cells[1] !== 'any' && cells[1] !== 'deleted') throw new Error(`"${cells[1]}" is not any or deleted: ${row}`);
    if (!cells[2]) throw new Error(`no reason given: ${row}`);
    rules.push({ glob, counts: cells[1], why: cells[2], re: globToRegExp(glob) });
  }
  if (!rules.length) throw new Error('the high-risk table is empty');
  return rules;
}

const STATUS = { A: 'added', M: 'modified', D: 'removed', R: 'renamed', C: 'copied', T: 'changed' };

// `git diff --name-status -M`: "M\tpath", "R097\told\tnew".
export function fromNameStatus(text) {
  return text.split('\n').filter(Boolean).map((line) => {
    const [code, a, b] = line.split('\t');
    const status = STATUS[code[0]] ?? 'modified';
    return b === undefined ? { path: a, status } : { path: b, previous: a, status };
  });
}

// GitHub's pulls/<n>/files entries.
export const fromPullFiles = (files) =>
  files.map((f) => ({ path: f.filename, status: f.status, ...(f.previous_filename ? { previous: f.previous_filename } : {}) }));

// Every high-risk path a change touches, once, with the first rule's reason.
// A rename touches the old path too, and for a "deleted" row it is the old path
// that is gone.
export function match(rules, changes) {
  const hits = new Map();
  const hit = (label, rule) => { if (!hits.has(label)) hits.set(label, { path: label, why: rule.why }); };
  for (const c of changes) {
    for (const rule of rules) {
      if (rule.counts === 'any') {
        for (const p of [c.path, c.previous].filter(Boolean)) if (rule.re.test(p)) hit(p, rule);
      } else if (c.status === 'removed' && rule.re.test(c.path)) {
        hit(`${c.path} (deleted)`, rule);
      } else if (c.status === 'renamed' && rule.re.test(c.previous)) {
        hit(`${c.previous} (renamed to ${c.path})`, rule);
      }
    }
  }
  return [...hits.values()];
}

// Tom's latest verdict on the head commit, if he gave one there.
function reviewApproves(reviews, headSha) {
  const verdicts = reviews.filter((r) => r.user === APPROVER && r.commit_id === headSha &&
    (r.state === 'APPROVED' || r.state === 'CHANGES_REQUESTED'));
  return verdicts.at(-1)?.state === 'APPROVED';
}

// Why a pull request on a public repo is an outsider's, or null: its author
// isn't Tom or the bot, or its head is in another repo (a fork). A private
// repo has no outsiders.
export function outsider(info) {
  if (info?.base?.repo?.private !== false) return null;
  const login = info.user?.login;
  if (!TRUSTED.includes(login)) return `opened by ${login ?? 'an unknown account'}`;
  if (info.head?.repo?.full_name !== info.base.repo.full_name) return `from ${info.head?.repo?.full_name ?? 'a deleted fork'}`;
  return null;
}

export function decide({ matches, reviews, headSha, outside = null }) {
  if (!matches.length && !outside) return { pass: true, why: 'no high-risk paths' };
  const head = headSha.slice(0, 7);
  if (reviewApproves(reviews, headSha)) return { pass: true, why: `approved: review by ${APPROVER} on ${head}` };
  return { pass: false, why: `needs Tom: an approving review by ${APPROVER} on ${head}` };
}

// A review reports no Risk tier of its own. A red Risk tier from one run blocks
// the merge whatever a later run says: Tom's approval once passed in its own
// run and the earlier red stayed (devkit PLAN.md › Agent identity). So the
// verdict stays with the pull_request_target runs, and a review reruns the
// newest of them on the head once it has finished, if its conclusion disagrees
// with the verdict now.
export function staleRun(runs, pass) {
  const run = runs.filter((r) => r.event === 'pull_request_target').sort((a, b) => a.id - b.id).at(-1);
  if (!run) return { action: 'none', why: 'no Risk tier run on the head commit yet' };
  if (run.status !== 'completed') return { action: 'wait', run, why: `run ${run.id} is ${run.status}` };
  if ((run.conclusion === 'success') === pass) return { action: 'none', run, why: `run ${run.id} already says ${run.conclusion}` };
  return { action: 'rerun', run, why: `run ${run.id} says ${run.conclusion}, the verdict now is ${pass ? 'pass' : 'fail'}` };
}

// CODEOWNERS can't say "only when deleted", so those rows stay with Risk tier.
export function codeowners(rules) {
  const lines = [
    '# Generated from REVIEW.md › High-risk paths: `node scripts/risk-tier.mjs codeowners > .github/CODEOWNERS`.',
    '# test/risk-tier.test.mjs fails when the two differ. GitHub requests Tom\'s review',
    '# from it. It enforces nothing: GitHub\'s code-owner rule needs at least one',
    '# required approval, which would put Tom on every PR, so Risk tier is the gate.',
    '# Rows that count only when a file is deleted (the tests) can\'t be said here.',
  ];
  for (const r of rules.filter((r) => r.counts === 'any')) {
    lines.push(`${r.glob.startsWith('**/') ? r.glob.slice(3) : `/${r.glob}`} @${APPROVER}`);
  }
  return `${lines.join('\n')}\n`;
}

const ROOT = path.resolve(import.meta.dirname, '..');

// The first of TABLES that holds the table: { file, text }.
export function readTable(root) {
  for (const file of TABLES) {
    const p = path.join(root, file);
    if (!existsSync(p)) continue;
    const text = readFileSync(p, 'utf8');
    if (text.includes(START)) return { file, text };
  }
  throw new Error(`no ${START} table in ${TABLES.join(' or ')}`);
}
const readRules = () => parseRules(readTable(ROOT).text);
const gh = (...args) => execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
const jsonLines = (text) => text.split('\n').filter(Boolean).map((l) => JSON.parse(l));
const sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);

function needPr({ repo, pr }, cmd) {
  if (!repo || !/^\d+$/.test(pr ?? '')) throw new Error(`${cmd} needs --repo <owner/name> --pr <n>`);
}

// The PR as Risk tier sees it: its head, labels, files and reviews.
function readPr(repo, pr) {
  const rules = readRules();
  const info = JSON.parse(gh('api', `repos/${repo}/pulls/${pr}`));
  const files = jsonLines(gh('api', '--paginate', `repos/${repo}/pulls/${pr}/files`,
    '--jq', '.[] | {filename, status, previous_filename}'));
  const reviews = jsonLines(gh('api', '--paginate', `repos/${repo}/pulls/${pr}/reviews`,
    '--jq', '.[] | {user: .user.login, state, commit_id}'));
  return {
    state: info.state, headSha: info.head.sha, labels: info.labels.map((l) => l.name),
    files, reviews, matches: match(rules, fromPullFiles(files)), outside: outsider(info),
  };
}

function check(args) {
  needPr(args, 'check');
  const { repo, pr } = args;
  const { headSha, labels, files, reviews, matches, outside } = readPr(repo, pr);
  const verdict = decide({ matches, reviews, headSha, outside });
  const out = [];
  out.push(matches.length
    ? `Risk tier: ${matches.length} high-risk path${matches.length === 1 ? '' : 's'} (${readTable(ROOT).file})`
    : `Risk tier: no high-risk paths in ${files.length} changed file${files.length === 1 ? '' : 's'}`);
  for (const m of matches) out.push(`  ${m.path}: ${m.why}`);
  if (outside) out.push(`  an outside pull request (${outside}): it never merges without Tom`);
  out.push(verdict.pass ? `pass: ${verdict.why}` : `FAIL: ${verdict.why}`);
  // A land script may label a high-risk PR needs-tom; Tom's approval answers
  // it. Only housekeeping, so a failure here doesn't touch the verdict.
  if (verdict.pass && (matches.length || outside) && labels.includes(NEEDS_TOM)) {
    try {
      gh('api', '-X', 'DELETE', `repos/${repo}/issues/${pr}/labels/${NEEDS_TOM}`);
      out.push(`${NEEDS_TOM} removed`);
    } catch {
      out.push(`${NEEDS_TOM} left on: GitHub refused to remove it`);
    }
  }
  console.log(out.join('\n'));
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `\`\`\`\n${out.join('\n')}\n\`\`\`\n`);
  return verdict.pass ? 0 : 1;
}

// After a review: rerun the newest Risk tier run on the head if the review has
// changed the verdict. One still running may have read the PR before the
// review, so it is waited for first. Its own exit is not a verdict: 0 when the
// run agrees or has been rerun, 1 when it couldn't get that far.
function rerun(args) {
  needPr(args, 'rerun');
  const { repo, pr } = args;
  const poll = Number(process.env.RISK_TIER_POLL_MS ?? 10_000);
  const until = Date.now() + Number(process.env.RISK_TIER_WAIT_MS ?? 600_000);
  const runs = (sha) => jsonLines(gh('api', `repos/${repo}/actions/workflows/risk-tier.yml/runs?head_sha=${sha}&event=pull_request_target&per_page=100`,
    '--jq', '.workflow_runs[] | {id, event, status, conclusion}'));
  for (;;) {
    const p = readPr(repo, pr);
    if (p.state !== 'open') { console.log(`Risk tier: #${pr} is ${p.state}, nothing to rerun`); return 0; }
    const verdict = decide(p);
    const next = staleRun(runs(p.headSha), verdict.pass);
    if (next.action === 'wait' && Date.now() < until) { sleep(poll); continue; }
    if (next.action === 'wait') { console.log(`Risk tier on ${p.headSha.slice(0, 7)}: ${next.why} still; not rerun`); return 1; }
    if (next.action === 'rerun') gh('api', '-X', 'POST', `repos/${repo}/actions/runs/${next.run.id}/rerun`);
    console.log(`Risk tier on ${p.headSha.slice(0, 7)}: ${verdict.pass ? 'pass' : 'fail'} (${verdict.why}); ${next.why}${next.action === 'rerun' ? ': rerun' : ''}`);
    return 0;
  }
}

function main(argv) {
  const [cmd, ...rest] = argv;
  const args = {};
  for (let i = 0; i < rest.length; i += 2) {
    if (!rest[i].startsWith('--') || rest[i + 1] === undefined) throw new Error(`bad argument: ${rest[i]}`);
    args[rest[i].slice(2)] = rest[i + 1];
  }
  if (cmd === 'match') {
    for (const m of match(readRules(), fromNameStatus(readFileSync(0, 'utf8')))) console.log(`${m.path}\t${m.why}`);
    return 0;
  }
  if (cmd === 'codeowners') { process.stdout.write(codeowners(readRules())); return 0; }
  if (cmd === 'check') return check(args);
  if (cmd === 'rerun') return rerun(args);
  console.error('usage: risk-tier.mjs match < name-status | check --repo <owner/name> --pr <n> | rerun --repo <owner/name> --pr <n> | codeowners');
  return 2;
}

// Compared as real paths: node resolves symlinks in a module's own URL, and
// macOS's TMPDIR is under /var, a symlink to /private/var.
if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) {
  try {
    process.exitCode = main(process.argv.slice(2));
  } catch (e) {
    console.error(`risk-tier: ${e.message}`);
    process.exitCode = 2;
  }
}
