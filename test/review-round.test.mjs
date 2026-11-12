// The reviewer's rounds (scripts/review-round.mjs): the marker, which round runs
// and on what, the verdict, fail-open, and the cap. The git cases run against a
// throwaway repo; the CLI runs against a stub gh.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { BOT, CAP, context, latestState, marker, plan, report, sinceReviewed } from '../scripts/review-round.mjs';

const SCRIPT = path.resolve(import.meta.dirname, '../scripts/review-round.mjs');
const HEAD = 'c'.repeat(40);
const finding = (o = {}) => ({ severity: 'Important', file: 'src/feed.mjs', line: 12, title: 'drops the last item', detail: 'a feed of one shows none', ...o });
const answer = (o = {}) => JSON.stringify({ summary: 'One bug.', findings: [], resolved: [], still_open: [], ...o });
const open1 = { id: 'r1-1', file: 'src/feed.mjs', line: 12, title: 'drops the last item' };

test('the state is the newest marker the reviewer wrote; anyone else\'s is ignored', () => {
  const m = (state) => `Round\n\n${marker(state)}`;
  assert.equal(latestState([]), null);
  assert.deepEqual(latestState([
    { user: BOT, body: m({ round: 1, reviewed: 'a', open: [] }) },
    { user: 'tomdebres', body: m({ round: 1, reviewed: HEAD, open: [] }) },
    { user: BOT, body: m({ round: 2, reviewed: 'b', open: [open1] }) },
    { user: BOT, body: 'no marker here' },
    { user: BOT, body: '<!-- claude-review {not json} -->' },
  ]), { round: 2, reviewed: 'b', open: [open1] });
  // A title that could close the HTML comment survives the round trip.
  const tricky = { round: 1, reviewed: 'a', open: [{ ...open1, title: 'a --> b <!-- c' }] };
  assert.doesNotMatch(marker(tricky).slice(4, -3), /-->|<!--/);
  assert.deepEqual(latestState([{ user: BOT, body: marker(tricky) }]), tricky);
});

test('which round: the whole PR first, then only what is new, then the cap', () => {
  const state = (o = {}) => ({ round: 1, reviewed: 'a'.repeat(40), open: [], ...o });
  assert.deepEqual(plan({ state: null, labels: [], head: HEAD, commits: null }), { mode: 'review', round: 1, open: [], commits: null });
  assert.deepEqual(plan({ state: state(), labels: [], head: HEAD, commits: ['d1'] }), { mode: 'review', round: 2, open: [], commits: ['d1'] });
  // The reviewed head is gone: the whole PR again, as the next round.
  assert.equal(plan({ state: state(), labels: [], head: HEAD, commits: null }).commits, null);
  assert.equal(plan({ state: state({ reviewed: HEAD, open: [open1] }), labels: [], head: HEAD, commits: null }).mode, 'same');
  assert.equal(plan({ state: state(), labels: [], head: HEAD, commits: [] }).mode, 'rebased');
  assert.equal(plan({ state: state({ round: CAP, open: [open1] }), labels: [], head: HEAD, commits: ['d1'] }).mode, 'cap');
  assert.equal(plan({ state: state({ round: 2 }), labels: [], head: HEAD, commits: ['d1'] }).round, 3);
  assert.equal(plan({ state: state(), labels: ['review-cap'], head: HEAD, commits: ['d1'] }).mode, 'capped');
  assert.equal(plan({ state: state({ round: 4, capped: true }), labels: [], head: HEAD, commits: ['d1'] }).mode, 'capped');
});

test('the cap is for rounds that don\'t converge: a push after a clean round 3 gets only its new commits reviewed', () => {
  const round3 = (open) => ({ round: CAP, reviewed: 'a'.repeat(40), open, verdict: 'x' });
  // Clean round 3, then a push: round 4 reviews just that commit.
  const p = plan({ state: round3([]), labels: [], head: HEAD, commits: ['d1'] });
  assert.deepEqual(p, { mode: 'review', round: 4, open: [], commits: ['d1'] });
  assert.match(context(p, HEAD), /^Round 4, on ccccccc\.\nReview only the commits new since round 3, with `git show <sha>`:\n- d1$/m);
  // ...or nothing new (a rebase): the clean verdict stands, no round spent.
  assert.equal(plan({ state: round3([]), labels: [], head: HEAD, commits: [] }).mode, 'rebased');
  // Round 3 with an Important finding still open, then a push: the cap.
  assert.equal(plan({ state: round3([open1]), labels: [], head: HEAD, commits: ['d1'] }).mode, 'cap');
  // A round past the cap that finds something: say the cap is next, and it is.
  const r = report({ plan: p, outcome: 'success', output: answer({ findings: [finding()] }), head: HEAD });
  assert.equal(r.code, 1);
  assert.match(r.body, /^\*\*Claude review, round 4\*\* on `ccccccc`: 1 Important finding open/m);
  assert.match(r.body, /^Round 4: if another push still leaves these open, the round cap stops this PR\./m);
  const after = latestState([{ user: BOT, body: r.body }]);
  assert.equal(plan({ state: after, labels: [], head: 'e'.repeat(40), commits: ['e1'] }).mode, 'cap');
  // A clean round past the cap passes like any other.
  assert.equal(report({ plan: p, outcome: 'success', output: answer(), head: HEAD }).code, 0);
});

test('the prompt\'s scope: the whole diff, or the new commits plus the open findings to recheck', () => {
  assert.match(context({ round: 1, open: [], commits: null }, HEAD), /^Round 1 of 3, on ccccccc\.\nReview the whole PR: `git diff origin\/main\.\.\.HEAD`/);
  const later = context({ round: 2, open: [open1], commits: ['d1', 'd2'] }, HEAD);
  assert.match(later, /^Review only the commits new since round 1, with `git show <sha>`:\n- d1\n- d2$/m);
  assert.match(later, /^- r1-1: src\/feed\.mjs:12: drops the last item$/m);
});

function repo(t) {
  const dir = mkdtempSync(path.join(tmpdir(), 'review-round-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const env = { ...process.env, GIT_AUTHOR_NAME: 'T', GIT_AUTHOR_EMAIL: 't@example.com', GIT_COMMITTER_NAME: 'T', GIT_COMMITTER_EMAIL: 't@example.com', GIT_CONFIG_NOSYSTEM: '1' };
  const git = (...args) => execFileSync('git', args, { cwd: dir, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  const commit = (file, text) => { writeFileSync(path.join(dir, file), text); git('add', file); git('commit', '-qm', file + text.trim()); return git('rev-parse', 'HEAD').trim(); };
  git('init', '-q', '-b', 'main');
  commit('base.txt', 'base\n');
  return { git, commit };
}

test('a later round reviews the commits pushed on top, or after a rebase only the new patches', (t) => {
  const { git, commit } = repo(t);
  git('switch', '-q', '-c', 'task');
  const a = commit('feed.txt', 'one\n');
  const b = commit('feed.txt', 'two\n');
  assert.deepEqual(sinceReviewed(git, { reviewed: a, head: b, base: 'main' }), [b]);

  // main moves on, and the task rebases onto it with one more commit.
  git('switch', '-q', 'main');
  commit('other.txt', 'meanwhile\n');
  git('switch', '-q', 'task');
  git('rebase', '-q', 'main');
  const c = commit('feed.txt', 'three\n');
  assert.deepEqual(sinceReviewed(git, { reviewed: b, head: c, base: 'main' }), [c]);
  // A pure rebase brings nothing new.
  assert.deepEqual(sinceReviewed(git, { reviewed: c, head: c, base: 'main' }), []);
  git('commit', '-q', '--amend', '-m', 'amended');
  const amended = git('rev-parse', 'HEAD').trim();
  assert.deepEqual(sinceReviewed(git, { reviewed: c, head: amended, base: 'main' }), []);   // same patch
  // A reviewed head that no longer exists anywhere: review the whole PR.
  assert.equal(sinceReviewed(git, { reviewed: 'f'.repeat(40), head: amended, base: 'main' }), null);
});

test('a finding: Important fails and is kept open by id; Nits and pre-existing bugs never block; style is dropped', () => {
  const p = { mode: 'review', round: 1, open: [], commits: null };
  let r = report({ plan: p, outcome: 'success', output: answer({ findings: [finding(), finding({ severity: 'Nit', title: 'odd log line' })] }), head: HEAD });
  assert.equal(r.code, 1);
  assert.match(r.body, /^\*\*Claude review, round 1 of 3\*\* on `ccccccc`: 1 Important finding open; this check stays red until it is fixed\.$/m);
  assert.match(r.body, /^### Important\n- \*\*src\/feed\.mjs:12\*\*: drops the last item\. a feed of one shows none$/m);
  assert.match(r.body, /^### Nit \(never blocks\)\n- \*\*src\/feed\.mjs:12\*\*: odd log line/m);
  assert.deepEqual(latestState([{ user: BOT, body: r.body }]),
    { round: 1, reviewed: HEAD, open: [open1], verdict: '1 open Important finding, 1 Nit' });

  r = report({ plan: p, outcome: 'success', output: answer({ findings: [finding({ severity: 'Nit' }), finding({ severity: 'Pre-existing' }), finding({ severity: 'Style' })] }), head: HEAD });
  assert.equal(r.code, 0);
  assert.match(r.body, /: no open Important findings\.$/m);
  assert.match(r.body, /^### Pre-existing \(already on main; never blocks\)$/m);
  assert.doesNotMatch(r.body, /Style/);
});

test('an earlier Important finding stays open until a round says it is resolved', () => {
  const p = { mode: 'review', round: 2, open: [open1], commits: ['d1'] };
  let r = report({ plan: p, outcome: 'success', output: answer({ still_open: [] }), head: HEAD });
  assert.equal(r.code, 1, 'unmentioned means still open');
  assert.match(r.body, /^### Still open from earlier rounds\n- r1-1 \*\*src\/feed\.mjs:12\*\*: drops the last item$/m);
  r = report({ plan: p, outcome: 'success', output: answer({ resolved: ['r1-1'] }), head: HEAD });
  assert.equal(r.code, 0);
  assert.match(r.body, /^### Fixed since the last round\n- r1-1 /m);
  assert.deepEqual(latestState([{ user: BOT, body: r.body }]).open, []);
  // Round 3 with something open says the cap is next.
  r = report({ plan: { ...p, round: 3 }, outcome: 'success', output: answer(), head: HEAD });
  assert.match(r.body, /^Round 3 of 3: if another push still leaves these open, the round cap stops this PR\./m);
});

test('the reviewer failing passes open with a warning: token, outage, timeout, turn limit, a malformed answer', () => {
  const p = { mode: 'review', round: 2, open: [open1], commits: ['d1'] };
  for (const [outcome, output] of [['failure', ''], ['cancelled', ''], ['success', ''], ['success', 'not json'], ['success', '{"summary":"x"}'], ['', '']]) {
    const r = report({ plan: p, outcome, output, head: HEAD });
    assert.equal(r.code, 0, `${outcome} ${output}`);
    assert.match(r.warning, /^review skipped: the reviewer /);
    assert.match(r.body, /^\*\*Claude review, round 2: skipped\*\* /);
    // No marker: the next push is still round 2, against the same open findings.
    assert.equal(latestState([{ user: BOT, body: r.body }]), null);
  }
});

test('after round 3: "round cap reached: rethink the approach", the label, red, and it stays that way', () => {
  const r = report({ plan: { mode: 'cap', round: 3, open: [] }, outcome: 'skipped', output: '', head: HEAD });
  assert.equal(r.code, 1);
  assert.equal(r.label, 'review-cap');
  assert.match(r.body, /^\*\*Claude review: round cap reached: rethink the approach\.\*\*/);
  const state = latestState([{ user: BOT, body: r.body }]);
  assert.equal(state.capped, true);
  assert.equal(plan({ state, labels: [], head: 'e'.repeat(40), commits: ['e'] }).mode, 'capped');
  assert.equal(report({ plan: { mode: 'capped', round: 4, open: [] }, outcome: 'skipped', head: HEAD }).code, 1);
});

test('the same head, or a rebase with nothing new, keeps the last verdict without a review', () => {
  assert.equal(report({ plan: { mode: 'same', round: 1, open: [open1] }, head: HEAD }).code, 1);
  assert.equal(report({ plan: { mode: 'same', round: 1, open: [] }, head: HEAD }).code, 0);
  const r = report({ plan: { mode: 'rebased', round: 2, open: [] }, head: HEAD });
  assert.equal(r.code, 0);
  assert.match(r.body, /no new changes since round 2/);
  assert.deepEqual(latestState([{ user: BOT, body: r.body }]), { round: 2, reviewed: HEAD, open: [], verdict: 'no open Important findings' });
});

// The two workflow steps against a stub gh.
function cli(t, comments = []) {
  const dir = mkdtempSync(path.join(tmpdir(), 'review-cli-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(path.join(dir, 'bin'));
  writeFileSync(path.join(dir, 'comments.jsonl'), comments.map((c) => JSON.stringify(c)).join('\n'));
  writeFileSync(path.join(dir, 'bin/gh'), String.raw`#!/bin/bash
echo "gh $*" >> "$STUB/calls"
case "$*" in
  *"issues/12/comments"*) cat "$STUB/comments.jsonl" ;;
  "api repos/tomdebres/devkit/pulls/12") echo '{"labels":[]}' ;;
  "pr comment 12 "*) while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cp "$2" "$STUB/posted.md"; shift; done ;;
  "pr edit 12 "*) ;;
  *) echo "unexpected $*" >&2; exit 1 ;;
esac
`);
  chmodSync(path.join(dir, 'bin/gh'), 0o755);
  writeFileSync(path.join(dir, 'out'), '');
  const base = { ...process.env, PATH: `${dir}/bin:${process.env.PATH}`, STUB: dir, RUNNER_TEMP: dir, GITHUB_OUTPUT: path.join(dir, 'out'),
    GITHUB_REPOSITORY: 'tomdebres/devkit', PR: '12', HEAD_SHA: HEAD };
  const step = (cmd, env = {}) => {
    const r = spawnSync('node', [SCRIPT, cmd], { env: { ...base, ...env }, encoding: 'utf8' });
    return { code: r.status, out: r.stdout + r.stderr };
  };
  const read = (f) => (existsSync(path.join(dir, f)) ? readFileSync(path.join(dir, f), 'utf8') : '');
  return { step, read };
}

test('plan, then report: round 1 asks for the whole diff; an Important finding posts the comment and fails', (t) => {
  const { step, read } = cli(t);
  let r = step('plan');
  assert.equal(r.code, 0, r.out);
  assert.match(read('out'), /^mode<<EOF_\w+\nreview\nEOF_\w+$/m);
  assert.match(read('out'), /Round 1 of 3, on ccccccc\.\nReview the whole PR/);
  r = step('report', { PLAN_OUTCOME: 'success', CLAUDE_OUTCOME: 'success', FINDINGS: answer({ findings: [finding()] }) });
  assert.equal(r.code, 1, r.out);
  assert.match(read('posted.md'), /^### Important$/m);
  assert.match(read('posted.md'), /<!-- claude-review \{"round":1,/);
  assert.match(read('calls'), /^gh pr comment 12 --repo tomdebres\/devkit --body-file /m);
});

test('report passes open when the plan step failed, and never exits 1 for its own crash', (t) => {
  const { step } = cli(t);
  let r = step('report', { PLAN_OUTCOME: 'failure' });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /^::warning::review skipped: planning the round failure; passing open$/m);
  // No plan file: a crash, which the workflow passes with a warning.
  r = step('report', { PLAN_OUTCOME: 'success' });
  assert.equal(r.code, 3, r.out);
  r = step('report', { PLAN_OUTCOME: 'success', CLAUDE_OUTCOME: 'failure' });
  assert.equal(r.code, 3, r.out);
  step('plan');
  r = step('report', { PLAN_OUTCOME: 'success', CLAUDE_OUTCOME: 'failure', FINDINGS: '' });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /^::warning::review skipped: the reviewer failure; passing open$/m);
});

test('the same head is not reviewed twice: plan says "same" and the verdict stands', (t) => {
  const { step, read } = cli(t, [{ user: BOT, body: `round 1\n\n${marker({ round: 1, reviewed: HEAD, open: [open1], verdict: 'x' })}` }]);
  assert.equal(step('plan').code, 0);
  assert.match(read('out'), /^mode<<EOF_\w+\nsame\n/m);
  assert.equal(step('report', { PLAN_OUTCOME: 'success', CLAUDE_OUTCOME: 'skipped' }).code, 1);
  assert.equal(read('posted.md'), '');
});
