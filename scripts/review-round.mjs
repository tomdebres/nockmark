#!/usr/bin/env node
// The reviewer's rounds (.github/workflows/claude-code-review.yml, REVIEW.md ›
// Findings). Claude judges; this does the bookkeeping, so the gate is a
// machine.
//
//   node scripts/review-round.mjs plan     # before Claude: which round, what to review
//   node scripts/review-round.mjs report   # after Claude: the comment, the label, the verdict
//
// State lives in a marker in the reviewer's own comment (github-actions[bot];
// a marker from anyone else is ignored): the round, the head it reviewed and
// the Important findings still open. Round 1 reviews the whole PR; later rounds
// only the commits since the last reviewed head, and whether the open findings
// were fixed. A push after round 3 that still left an Important finding open
// gets "round cap reached", label review-cap, red; after a clean round it gets
// one more diff-only round. report exits 1 only for an open Important finding
// or the cap. Anything
// else going wrong (token, outage, timeout, a malformed answer) passes with a
// warning, so an outage never blocks every merge.
import { appendFileSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const CAP = 3;
export const BOT = 'github-actions[bot]';
export const CAP_LABEL = 'review-cap';
const SEVERITIES = ['Important', 'Nit', 'Pre-existing'];
const MARK = /<!-- claude-review (\{.*?\}) -->/g;

// The newest state the reviewer itself wrote.
export function latestState(comments) {
  let state = null;
  for (const c of comments) {
    if (c.user !== BOT) continue;
    for (const m of c.body.matchAll(MARK)) {
      try { state = JSON.parse(m[1]); } catch { /* a damaged marker is no marker */ }
    }
  }
  return state;
}

// JSON inside an HTML comment: nothing in it may close the comment.
export const marker = (state) =>
  `<!-- claude-review ${JSON.stringify(state).replace(/</g, '\\u003c').replace(/>/g, '\\u003e')} -->`;

// What a later round reviews: the commits since the head it last reviewed.
// After a push on top, that is reviewed..head. After a rebase, it is the
// commits whose patch the reviewed head didn't have (git cherry). If the
// reviewed head can't be found, the whole PR again.
export function sinceReviewed(git, { reviewed, head, base }) {
  const lines = (s) => s.split('\n').filter(Boolean);
  try { git('cat-file', '-e', `${reviewed}^{commit}`); } catch {
    try { git('fetch', '--quiet', 'origin', reviewed); } catch { return null; }
  }
  try {
    git('merge-base', '--is-ancestor', reviewed, head);
    return lines(git('rev-list', '--reverse', `${reviewed}..${head}`, `^${base}`));
  } catch { /* rebased */ }
  const limit = git('merge-base', base, head).trim();
  return lines(git('cherry', reviewed, head, limit)).filter((l) => l.startsWith('+ ')).map((l) => l.slice(2));
}

// The round to run, from the state and what changed since. The cap is for
// rounds that don't converge: it fires only when the last round, at or past
// the third, still had an Important finding open. A clean PR that gets a later
// push has just the new commits reviewed. The cap is final for the PR: a fresh
// approach is a fresh PR.
export function plan({ state, labels, head, commits }) {
  if (labels.includes(CAP_LABEL) || state?.capped) return { mode: 'capped', round: state?.round ?? CAP, open: state?.open ?? [] };
  if (!state) return { mode: 'review', round: 1, open: [], commits: null };
  if (state.reviewed === head) return { mode: 'same', round: state.round, open: state.open };
  if (commits && commits.length === 0) return { mode: 'rebased', round: state.round, open: state.open };
  if (state.round >= CAP && state.open.length) return { mode: 'cap', round: state.round, open: state.open };
  return { mode: 'review', round: state.round + 1, open: state.open, commits };
}

const where = (f) => `${f.file}${f.line ? `:${f.line}` : ''}`;
// "round 2 of 3"; past the third (after a clean one), just "round 4".
const ofCap = (round) => (round <= CAP ? `${round} of ${CAP}` : `${round}`);

// This round's scope, for the prompt.
export function context(p, head) {
  const out = [`Round ${ofCap(p.round)}, on ${head.slice(0, 7)}.`];
  if (!p.commits) {
    out.push('Review the whole PR: `git diff origin/main...HEAD` (commits: `git log origin/main..HEAD`).');
  } else {
    out.push(`Review only the commits new since round ${p.round - 1}, with \`git show <sha>\`:`);
    for (const c of p.commits) out.push(`- ${c}`);
    out.push('The rest of the PR (`git diff origin/main...HEAD`) was reviewed already: read it for context only.');
  }
  if (p.open.length) {
    out.push('', 'Important findings still open from earlier rounds. For each id, say "resolved" if the current code fixes it (or it was wrong), "still_open" if not:');
    for (const f of p.open) out.push(`- ${f.id}: ${where(f)}: ${f.title}`);
  }
  return out.join('\n');
}

const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;
const bullet = (f) => `- **${where(f)}**: ${f.title}${f.detail ? `. ${f.detail}` : ''}`;

// The verdict, the comment and the labels, from the plan and Claude's answer.
export function report({ plan: p, outcome, output, head }) {
  const done = (code, body, state, extra = {}) => ({ code, body: body && state ? `${body}\n\n${marker(state)}` : body, ...extra });
  const keep = { round: p.round, reviewed: head, open: p.open, verdict: verdictOf(p.open) };

  if (p.mode === 'capped') return done(1, null, null, { log: `round cap reached earlier (${CAP_LABEL}): rethink the approach` });
  if (p.mode === 'same') return done(p.open.length ? 1 : 0, null, null, { log: `already reviewed ${head.slice(0, 7)}: ${verdictOf(p.open)}` });
  if (p.mode === 'rebased') {
    return done(p.open.length ? 1 : 0, `**Claude review**: rebased onto main, no new changes since round ${p.round}. ${sentence(p.open)}`, keep);
  }
  if (p.mode === 'cap') {
    return done(1, `**Claude review: round cap reached: rethink the approach.** ${CAP} rounds haven't converged. ` +
      `Close this PR and start again from main with a different approach, rather than another fix on top.`,
    { ...keep, round: CAP + 1, capped: true, verdict: 'round cap reached' }, { label: CAP_LABEL });
  }

  // A review round. Anything short of a well-formed answer is the reviewer
  // failing, not the code: pass, and say so.
  let answer = null;
  if (outcome === 'success') { try { answer = JSON.parse(output || 'null'); } catch { /* below */ } }
  const ok = answer && Array.isArray(answer.findings) && Array.isArray(answer.resolved);
  if (!ok) {
    const why = outcome !== 'success' ? `the reviewer ${outcome || 'did not run'}` : 'the reviewer gave no well-formed answer';
    return done(0, `**Claude review, round ${p.round}: skipped** (${why}: token, outage, timeout or turn limit). ` +
      'Passing open, so an outage doesn\'t block every merge. The next push reviews again.', null, { warning: `review skipped: ${why}` });
  }

  const findings = answer.findings.filter((f) => SEVERITIES.includes(f.severity) && f.title);
  const resolved = new Set(answer.resolved);
  const fixed = p.open.filter((f) => resolved.has(f.id));
  const still = p.open.filter((f) => !resolved.has(f.id));
  const fresh = findings.filter((f) => f.severity === 'Important')
    .map((f, i) => ({ id: `r${p.round}-${i + 1}`, file: f.file, line: f.line ?? null, title: f.title }));
  const open = [...still, ...fresh];
  const nits = findings.filter((f) => f.severity === 'Nit'), old = findings.filter((f) => f.severity === 'Pre-existing');

  const verdict = verdictOf(open, nits.length);
  const body = [`**Claude review, round ${ofCap(p.round)}** on \`${head.slice(0, 7)}\`: ${sentence(open)}`];
  if (answer.summary) body.push('', answer.summary);
  const section = (title, items, fmt = bullet) => { if (items.length) body.push('', `### ${title}`, ...items.map(fmt)); };
  section('Important', findings.filter((f) => f.severity === 'Important'));
  section('Still open from earlier rounds', still, (f) => `- ${f.id} **${where(f)}**: ${f.title}`);
  section('Fixed since the last round', fixed, (f) => `- ${f.id} **${where(f)}**: ${f.title}`);
  section('Nit (never blocks)', nits);
  section('Pre-existing (already on main; never blocks)', old);
  if (open.length && p.round >= CAP) {
    body.push('', `Round ${ofCap(p.round)}: if another push still leaves these open, the round cap stops this PR. If the approach is fighting the reviewer, rethink it now.`);
  }
  return done(open.length ? 1 : 0, body.join('\n'), { round: p.round, reviewed: head, open, verdict });
}

function verdictOf(open, nits = 0) {
  const n = nits ? `, ${plural(nits, 'Nit')}` : '';
  return open.length ? `${plural(open.length, 'open Important finding')}${n}` : `no open Important findings${n}`;
}
const sentence = (open) => (open.length
  ? `${plural(open.length, 'Important finding')} open; this check stays red until ${open.length === 1 ? 'it is' : 'they are'} fixed.`
  : 'no open Important findings.');

// The workflow's side: GitHub, git and the step's files.
const env = process.env;
const gh = (...args) => execFileSync('gh', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });
const git = (...args) => execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
const jsonLines = (text) => text.split('\n').filter(Boolean).map((l) => JSON.parse(l));
const planFile = () => path.join(env.RUNNER_TEMP ?? '/tmp', 'review-plan.json');
const output = (name, value) => {
  if (!env.GITHUB_OUTPUT) return;
  const eof = `EOF_${Math.random().toString(36).slice(2)}`;
  appendFileSync(env.GITHUB_OUTPUT, `${name}<<${eof}\n${value}\n${eof}\n`);
};

function cliPlan() {
  const { GITHUB_REPOSITORY: repo, PR: pr, HEAD_SHA: head } = env;
  const comments = jsonLines(gh('api', '--paginate', `repos/${repo}/issues/${pr}/comments`, '--jq', '.[] | {user: .user.login, body}'));
  const labels = JSON.parse(gh('api', `repos/${repo}/pulls/${pr}`)).labels.map((l) => l.name);
  const state = latestState(comments);
  const commits = state && state.reviewed !== head
    ? sinceReviewed(git, { reviewed: state.reviewed, head, base: 'refs/remotes/origin/main' })
    : null;
  const p = plan({ state, labels, head, commits });
  writeFileSync(planFile(), JSON.stringify(p));
  output('mode', p.mode);
  output('context', p.mode === 'review' ? context(p, head) : '');
  console.log(`round ${p.round}: ${p.mode}${p.commits ? ` (${plural(p.commits.length, 'new commit')})` : ''}`);
}

function cliReport() {
  const { GITHUB_REPOSITORY: repo, PR: pr, HEAD_SHA: head } = env;
  if (env.PLAN_OUTCOME !== 'success') {
    console.log(`::warning::review skipped: planning the round ${env.PLAN_OUTCOME || 'did not run'}; passing open`);
    return 0;
  }
  const r = report({ plan: JSON.parse(readFileSync(planFile(), 'utf8')), outcome: env.CLAUDE_OUTCOME, output: env.FINDINGS, head });
  // The verdict is decided; failing to post it doesn't change it.
  try {
    if (r.body) {
      writeFileSync(planFile() + '.comment', r.body);
      gh('pr', 'comment', pr, '--repo', repo, '--body-file', planFile() + '.comment');
    }
    if (r.label) gh('pr', 'edit', pr, '--repo', repo, '--add-label', r.label);
  } catch (e) {
    console.log(`::warning::could not post the review: ${e.message.split('\n')[0]}`);
  }
  if (r.warning) console.log(`::warning::${r.warning}; passing open`);
  if (r.log) console.log(r.log);
  if (r.body) console.log(r.body.replace(/\n\n<!-- claude-review .* -->$/, ''));
  return r.code;
}

const invoked = process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
if (invoked) {
  const cmd = process.argv[2];
  try {
    if (cmd === 'plan') { cliPlan(); process.exitCode = 0; } else if (cmd === 'report') process.exitCode = cliReport();
    else { console.error('usage: review-round.mjs plan | report'); process.exitCode = 2; }
  } catch (e) {
    // Never 1: that is reserved for findings.
    console.error(`review-round: ${e.stack ?? e.message}`);
    process.exitCode = 3;
  }
}
