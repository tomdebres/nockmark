// Risk tier (scripts/risk-tier.mjs), from devkit's templates/risk-tier.test.mjs
// (kit-install --risk-tier): change it there, not here. First this repo's own
// setup: its high-risk table (REVIEW.md, or .github/high-risk-paths.md in a
// public repo), the CODEOWNERS made from it and the
// two workflows. Then the script against a fixed table: which paths it matches,
// the verdict, and the "Risk tier" job and a review's rerun end to end against
// a stub gh on PATH.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  APPROVER, codeowners, decide, fromNameStatus, fromPullFiles, globToRegExp, match, outsider, parseRules, readTable, staleRun,
} from '../scripts/risk-tier.mjs';

const ROOT = path.resolve(import.meta.dirname, '..');
const SCRIPT = path.join(ROOT, 'scripts/risk-tier.mjs');
const HEAD = 'a'.repeat(40), OLD = 'b'.repeat(40);
const REPO = 'owner/repo';

// ---- this repo ---------------------------------------------------------------

const table = readTable(ROOT);
const ours = parseRules(table.text);

test('the high-risk table is there, every row with a reason', () => {
  for (const r of ours) assert.ok(r.why.length > 20, `${r.glob}: "${r.why}"`);
});

test('every row names files that exist: a rename can\'t quietly drop a path off the list', () => {
  const files = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard'], { cwd: ROOT, encoding: 'utf8' })
    .split('\n').filter(Boolean);
  for (const r of ours) {
    assert.ok(files.some((f) => r.re.test(f)), `${r.glob} matches no file in the repo`);
  }
});

test('the gate guards itself: its table, script, workflows and CODEOWNERS are high-risk', () => {
  for (const p of [table.file, 'scripts/risk-tier.mjs', '.github/workflows/risk-tier.yml',
    '.github/workflows/risk-tier-review.yml', '.github/CODEOWNERS']) {
    assert.ok(ours.some((r) => r.counts === 'any' && r.re.test(p)), `${p} isn't on the list`);
  }
});

test('.github/CODEOWNERS is what the table generates', () => {
  assert.equal(readFileSync(path.join(ROOT, '.github/CODEOWNERS'), 'utf8'), codeowners(ours),
    'regenerate: node scripts/risk-tier.mjs codeowners > .github/CODEOWNERS');
});

test('the workflow runs main\'s code for any base, and reruns when the base changes', () => {
  const yml = readFileSync(path.join(ROOT, '.github/workflows/risk-tier.yml'), 'utf8');
  assert.match(yml, /^ {2}pull_request_target:$/m);
  assert.match(yml, /^ {4}name: Risk tier$/m);
  assert.match(yml, /^ {10}ref: \$\{\{ github\.event\.repository\.default_branch \}\}$/m);
  assert.doesNotMatch(yml, /base\.ref|head\.ref|head\.sha|refs\/pull/);
  const types = yml.slice(yml.indexOf('pull_request_target:'), yml.indexOf('concurrency:'));
  for (const t of ['opened', 'synchronize', 'reopened', 'edited', 'ready_for_review']) assert.match(types, new RegExp(`- ${t}$`, 'm'));
  // Labels decide nothing, so they start no run.
  assert.doesNotMatch(types, /labeled/);
});

test('a review reports no Risk tier of its own; its workflow reruns main\'s instead', () => {
  const dir = path.join(ROOT, '.github/workflows');
  const tier = readFileSync(path.join(dir, 'risk-tier.yml'), 'utf8');
  assert.doesNotMatch(tier, /^ {2}pull_request_review:/m);
  const yml = readFileSync(path.join(dir, 'risk-tier-review.yml'), 'utf8');
  assert.match(yml, /^ {2}pull_request_review:\n {4}types:\n {6}- submitted\n {6}- dismissed\n/m);
  assert.match(yml, /^ {2}actions: write$/m);
  assert.match(yml, /^ {10}ref: \$\{\{ github\.event\.repository\.default_branch \}\}$/m);
  assert.match(yml, /node scripts\/risk-tier\.mjs rerun --repo "\$GITHUB_REPOSITORY" --pr "\$PR"$/m);
  assert.match(yml, /group: risk-tier-review-/);
  // No other job may answer for the required check.
  for (const f of readdirSync(dir).filter((f) => /\.ya?ml$/.test(f) && f !== 'risk-tier.yml')) {
    assert.doesNotMatch(readFileSync(path.join(dir, f), 'utf8'), /^\s*name: Risk tier\s*$/m, f);
  }
});

// ---- the script, against a fixed table ---------------------------------------

const TABLE = `# Review

<!-- high-risk-paths -->
| Path | Counts | Why |
|---|---|---|
| \`src/auth.mjs\` | any | Who the caller is. |
| \`schema.sql\` | any | The live database's shape. |
| \`**/*.entitlements\` | any | What the signed app may do. |
| \`.github/**\` | any | The gates themselves. |
| \`REVIEW.md\` | any | This list. |
| \`test/**\` | deleted | A deleted test is a weaker spec. |
| \`ios/*Tests/**\` | deleted | A deleted iOS test is a weaker spec. |
<!-- /high-risk-paths -->
`;
const rules = parseRules(TABLE);
const touched = (...lines) => match(rules, fromNameStatus(lines.join('\n')));
const paths = (...lines) => touched(...lines).map((m) => m.path);

// A copy of the script beside the fixed table, as a checkout would hold them.
function fixture(t, table = 'REVIEW.md') {
  const dir = mkdtempSync(path.join(tmpdir(), 'risk-tier-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(path.join(dir, 'repo/scripts'), { recursive: true });
  mkdirSync(path.join(dir, 'repo/.github'), { recursive: true });
  writeFileSync(path.join(dir, 'repo/scripts/risk-tier.mjs'), readFileSync(SCRIPT));
  writeFileSync(path.join(dir, 'repo', table), TABLE);
  return { dir, script: path.join(dir, 'repo/scripts/risk-tier.mjs') };
}

test('a table that is not three columns of any/deleted with a reason is refused', () => {
  const block = (rows) => `<!-- high-risk-paths -->\n| Path | Counts | Why |\n|---|---|---|\n${rows}\n<!-- /high-risk-paths -->`;
  assert.equal(parseRules(block('| `a.txt` | any | Because it matters. |')).length, 1);
  assert.throws(() => parseRules('no table'), /no <!-- high-risk-paths -->/);
  assert.throws(() => parseRules(block('| `a.txt` | any |')), /three-column/);
  assert.throws(() => parseRules(block('| `a.txt` | sometimes | Because. |')), /not any or deleted/);
  assert.throws(() => parseRules(block('| `a.txt` | any | |')), /no reason/);
  assert.throws(() => parseRules(block('| `/a.txt` | any | Because. |')), /root-relative/);
  assert.throws(() => parseRules(block('')), /empty/);
});

test('globs: * stays in one directory, ** crosses any number, **/ may be none', () => {
  const yes = (g, p) => assert.ok(globToRegExp(g).test(p), `${g} ~ ${p}`);
  const no = (g, p) => assert.ok(!globToRegExp(g).test(p), `${g} !~ ${p}`);
  yes('schema.sql', 'schema.sql'); no('schema.sql', 'test/schema.sql'); no('schema.sql', 'schema.sqlx');
  yes('.github/**', '.github/workflows/ci.yml'); yes('.github/**', '.github/CODEOWNERS'); no('.github/**', 'x.github/a');
  yes('**/*.entitlements', 'ios/Watch/Watch.entitlements'); yes('**/*.entitlements', 'a.entitlements');
  no('**/*.entitlements', 'ios/a.entitlements.bak');
  yes('ios/*Tests/**', 'ios/AppTests/DayTests.swift'); yes('ios/*Tests/**', 'ios/AppUITests/a/b.swift');
  no('ios/*Tests/**', 'ios/App/Tests/x.swift'); no('src/?.mjs', 'src/ab.mjs'); yes('src/?.mjs', 'src/a.mjs');
  yes('scripts/run-*.sh', 'scripts/run-a.sh'); no('scripts/run-*.sh', 'scripts/sub/run-a.sh');
});

test('an ordinary change matches nothing; a high-risk one is listed with its reason', () => {
  assert.deepEqual(touched('M\tsrc/server.mjs', 'A\tsrc/new.mjs', 'M\tios/App/TodayView.swift', 'M\tREADME.md'), []);
  const [m] = touched('M\tsrc/server.mjs', 'M\tschema.sql');
  assert.equal(m.path, 'schema.sql');
  assert.match(m.why, /live database/);
  assert.deepEqual(paths('M\tios/Widgets/Widgets.entitlements', 'M\t.github/workflows/ci.yml'),
    ['ios/Widgets/Widgets.entitlements', '.github/workflows/ci.yml']);
  // Once per path, however many rows match it.
  assert.deepEqual(paths('M\tschema.sql', 'M\tschema.sql'), ['schema.sql']);
});

test('tests count only when deleted or renamed away; adding or editing one is ordinary', () => {
  assert.deepEqual(paths('A\ttest/new.test.mjs', 'M\ttest/auth.test.mjs', 'M\tios/AppTests/DayTests.swift'), []);
  assert.deepEqual(paths('D\ttest/auth.test.mjs'), ['test/auth.test.mjs (deleted)']);
  assert.deepEqual(paths('D\tios/AppUITests/SmokeUITests.swift'), ['ios/AppUITests/SmokeUITests.swift (deleted)']);
  assert.deepEqual(paths('R100\ttest/auth.test.mjs\tdocs/auth.test.mjs'), ['test/auth.test.mjs (renamed to docs/auth.test.mjs)']);
});

test('a high-risk file renamed away still counts, by its old path', () => {
  assert.deepEqual(paths('R095\tsrc/auth.mjs\tsrc/who.mjs'), ['src/auth.mjs']);
  assert.deepEqual(paths('R100\tsrc/a.mjs\tsrc/auth.mjs'), ['src/auth.mjs']);
});

test('GitHub\'s file list reads the same as git\'s', () => {
  const files = [
    { filename: 'src/server.mjs', status: 'modified' },
    { filename: 'test/x.test.mjs', status: 'removed' },
    { filename: 'src/who.mjs', status: 'renamed', previous_filename: 'src/auth.mjs' },
  ];
  assert.deepEqual(match(rules, fromPullFiles(files)).map((m) => m.path), ['test/x.test.mjs (deleted)', 'src/auth.mjs']);
});

test('the verdict: a high-risk path needs Tom\'s review of the head; an ordinary path passes', () => {
  const matches = touched('M\tschema.sql');
  assert.deepEqual(decide({ matches, reviews: [], headSha: HEAD }), { pass: false, why: `needs Tom: an approving review by ${APPROVER} on aaaaaaa` });
  assert.deepEqual(decide({ matches, reviews: [{ user: APPROVER, state: 'APPROVED', commit_id: HEAD }], headSha: HEAD }),
    { pass: true, why: `approved: review by ${APPROVER} on aaaaaaa` });
  assert.deepEqual(decide({ matches: [], reviews: [], headSha: HEAD }), { pass: true, why: 'no high-risk paths' });
});

test('on a public repo an outsider\'s PR needs Tom\'s review of the head, whatever it touches', () => {
  const repo = (priv) => ({ full_name: REPO, private: priv });
  const pr = (login, head = REPO, priv = false) => ({ user: { login }, base: { repo: repo(priv) }, head: { repo: head && { full_name: head } } });
  assert.equal(outsider(pr('tomdebres-agent')), null);
  assert.equal(outsider(pr(APPROVER)), null);
  assert.equal(outsider(pr('someone')), 'opened by someone');
  assert.equal(outsider(pr('dependabot[bot]')), 'opened by dependabot[bot]');
  assert.equal(outsider(pr('tomdebres-agent', 'someone/repo')), 'from someone/repo');
  assert.equal(outsider(pr('tomdebres-agent', null)), 'from a deleted fork');
  // A private repo, or a PR GitHub didn't describe, has no outsiders.
  assert.equal(outsider(pr('someone', 'someone/repo', true)), null);
  assert.equal(outsider({ head: { sha: HEAD } }), null);
  const outside = 'opened by someone';
  assert.deepEqual(decide({ matches: [], reviews: [], headSha: HEAD, outside }), { pass: false, why: `needs Tom: an approving review by ${APPROVER} on aaaaaaa` });
  assert.equal(decide({ matches: [], reviews: [{ user: APPROVER, state: 'APPROVED', commit_id: HEAD }], headSha: HEAD, outside }).pass, true);
  assert.equal(decide({ matches: [], reviews: [{ user: APPROVER, state: 'APPROVED', commit_id: OLD }], headSha: HEAD, outside }).pass, false);
});

test('a review approves only from the approver, on the head commit, as the latest verdict there', () => {
  const matches = touched('M\tschema.sql');
  const r = (o) => ({ user: APPROVER, state: 'APPROVED', commit_id: HEAD, ...o });
  const pass = (...reviews) => decide({ matches, reviews, headSha: HEAD }).pass;
  assert.equal(pass(r()), true);
  assert.equal(pass(r({ commit_id: OLD })), false);
  assert.equal(pass(r({ user: 'tomdebres-agent' })), false);
  assert.equal(pass(r({ state: 'DISMISSED' })), false);
  assert.equal(pass(r({ state: 'COMMENTED' })), false);
  assert.equal(pass(r(), r({ state: 'CHANGES_REQUESTED' })), false);
  assert.equal(pass(r({ state: 'CHANGES_REQUESTED' }), r()), true);
  assert.equal(pass(r(), r({ state: 'COMMENTED' })), true);
});

test('CODEOWNERS names the approver on every "any" row and leaves the deleted-only rows to Risk tier', () => {
  const generated = codeowners(rules);
  assert.match(generated, new RegExp(`^/schema\\.sql @${APPROVER}$`, 'm'));
  assert.match(generated, new RegExp(`^\\*\\.entitlements @${APPROVER}$`, 'm'));
  assert.match(generated, new RegExp(`^/\\.github/\\*\\* @${APPROVER}$`, 'm'));
  assert.doesNotMatch(generated, /^\/?test\//m);
  assert.doesNotMatch(generated, /Tests/);
});

// The job as GitHub runs it: gh answers from files, and logs every call.
// `runs` is what each successive look at the Risk tier runs finds; the last
// answer repeats.
function job(t, { files, labels = [], reviews = [], state = 'open', runs = [[]], cmd = 'check', refuse = false, pr = {}, table = 'REVIEW.md' }) {
  const { dir, script } = fixture(t, table);
  mkdirSync(path.join(dir, 'bin'));
  const lines = (rows) => rows.map((x) => JSON.stringify(x)).join('\n');
  writeFileSync(path.join(dir, 'pr.json'), JSON.stringify({ state, labels: labels.map((name) => ({ name })), ...pr, head: { sha: HEAD, ...pr.head } }));
  writeFileSync(path.join(dir, 'files.jsonl'), lines(files));
  writeFileSync(path.join(dir, 'reviews.jsonl'), lines(reviews));
  runs.forEach((r, i) => writeFileSync(path.join(dir, `runs-${i + 1}.jsonl`), lines(r)));
  if (refuse) writeFileSync(path.join(dir, 'refuse'), '');
  writeFileSync(path.join(dir, 'bin/gh'), String.raw`#!/bin/bash
echo "gh $*" >> "$STUB/calls"
case "$*" in
  "api repos/${REPO}/actions/workflows/risk-tier.yml/runs?head_sha=${HEAD}&event=pull_request_target&per_page=100 "*)
    n=$(( $(cat "$STUB/looks" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$STUB/looks"
    while [ ! -f "$STUB/runs-$n.jsonl" ]; do n=$((n - 1)); done
    cat "$STUB/runs-$n.jsonl" ;;
  *"pulls/12/files"*) cat "$STUB/files.jsonl" ;;
  *"pulls/12/reviews"*) cat "$STUB/reviews.jsonl" ;;
  "api repos/${REPO}/pulls/12") cat "$STUB/pr.json" ;;
  "api -X "*) if [ -f "$STUB/refuse" ]; then echo "gh: HTTP 403" >&2; exit 1; fi ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
`);
  chmodSync(path.join(dir, 'bin/gh'), 0o755);
  return (...extra) => {
    const env = {
      ...process.env, PATH: `${dir}/bin:${process.env.PATH}`, STUB: dir, GITHUB_STEP_SUMMARY: path.join(dir, 'summary'),
      RISK_TIER_POLL_MS: '1', RISK_TIER_WAIT_MS: '2000',
    };
    const r = spawnSync('node', [script, cmd, '--repo', REPO, '--pr', '12', ...extra], { env, encoding: 'utf8' });
    const read = (f) => (existsSync(path.join(dir, f)) ? readFileSync(path.join(dir, f), 'utf8') : '');
    return { code: r.status, out: r.stdout + r.stderr, calls: read('calls'), summary: read('summary') };
  };
}
const approved = [{ user: APPROVER, state: 'APPROVED', commit_id: HEAD }];

test('the job fails on a high-risk path without approval, and lists what it matched', (t) => {
  const run = job(t, { files: [{ filename: 'schema.sql', status: 'modified' }, { filename: 'src/server.mjs', status: 'modified' }] });
  const { code, out, summary } = run();
  assert.equal(code, 1, out);
  assert.match(out, /^Risk tier: 1 high-risk path \(REVIEW\.md\)$/m);
  assert.match(out, /^ {2}schema\.sql: The live database's shape/m);
  assert.match(out, /^FAIL: needs Tom: /m);
  assert.match(summary, /schema\.sql/);
});

test('the job passes on Tom\'s review of the head, and takes needs-tom off; an ordinary PR passes as it is', (t) => {
  const files = [{ filename: 'src/auth.mjs', status: 'modified' }];
  let r = job(t, { files, reviews: approved, labels: ['needs-tom'] })();
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, new RegExp(`^pass: approved: review by ${APPROVER} on aaaaaaa\\nneeds-tom removed$`, 'm'));
  assert.match(r.calls, /^gh api -X DELETE repos\/owner\/repo\/issues\/12\/labels\/needs-tom$/m);
  // Taking the label off is housekeeping: GitHub refusing it changes no verdict.
  r = job(t, { files, reviews: approved, labels: ['needs-tom'], refuse: true })();
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /^needs-tom left on: GitHub refused to remove it$/m);

  r = job(t, { files: [{ filename: 'src/server.mjs', status: 'modified' }, { filename: 'test/x.test.mjs', status: 'added' }] })();
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /^Risk tier: no high-risk paths in 2 changed files$/m);
  assert.doesNotMatch(r.calls, /-X/);
});

test('a label passes nothing, and nor does an approval of an earlier commit', (t) => {
  const files = [{ filename: 'REVIEW.md', status: 'modified' }];
  const r = job(t, { files, labels: ['tom-approved', 'needs-tom'] })();
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, new RegExp(`^FAIL: needs Tom: an approving review by ${APPROVER} on aaaaaaa$`, 'm'));
  assert.doesNotMatch(r.calls, /-X/);
  const old = job(t, { files, reviews: [{ user: APPROVER, state: 'APPROVED', commit_id: OLD }] })();
  assert.equal(old.code, 1, old.out);
});

test('a public checkout reads its own table even beside a REVIEW.md with one (a link into the notes)', (t) => {
  const { dir } = fixture(t, '.github/high-risk-paths.md');
  writeFileSync(path.join(dir, 'repo/REVIEW.md'), TABLE.replace('schema.sql', 'notes-only.sql'));
  assert.equal(readTable(path.join(dir, 'repo')).file, '.github/high-risk-paths.md');
  rmSync(path.join(dir, 'repo/.github/high-risk-paths.md'));
  assert.equal(readTable(path.join(dir, 'repo')).file, 'REVIEW.md');
});

test('a public repo\'s table is .github/high-risk-paths.md, and an outsider\'s ordinary PR fails until Tom approves it', (t) => {
  const files = [{ filename: 'src/server.mjs', status: 'modified' }];
  const pr = { user: { login: 'someone' }, base: { repo: { full_name: REPO, private: false } }, head: { repo: { full_name: 'someone/repo' } } };
  let r = job(t, { files, pr, table: '.github/high-risk-paths.md' })();
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /^ {2}an outside pull request \(opened by someone\): it never merges without Tom$/m);
  assert.match(r.out, /^FAIL: needs Tom: /m);
  r = job(t, { files: [{ filename: 'schema.sql', status: 'modified' }], pr, table: '.github/high-risk-paths.md' })();
  assert.match(r.out, /^Risk tier: 1 high-risk path \(\.github\/high-risk-paths\.md\)$/m);
  r = job(t, { files, pr, reviews: approved, table: '.github/high-risk-paths.md' })();
  assert.equal(r.code, 0, r.out);
  // The bot's own PR on the same public repo is ordinary.
  r = job(t, { files, pr: { ...pr, user: { login: 'tomdebres-agent' }, head: { repo: { full_name: REPO } } }, table: '.github/high-risk-paths.md' })();
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /^pass: no high-risk paths$/m);
});

const run = (id, conclusion, { status = 'completed', event = 'pull_request_target' } = {}) => ({ id, event, status, conclusion });

test('which run a review reruns: the newest pull_request_target one, finished, when it disagrees', () => {
  assert.equal(staleRun([], true).action, 'none');
  // Tom approved after a red: rerun that run, not the cancelled older one, and
  // never a run from another event.
  const runs = [run(1, 'cancelled'), run(2, 'failure'), run(3, 'success', { event: 'pull_request_review' })];
  assert.deepEqual([staleRun(runs, true).action, staleRun(runs, true).run.id], ['rerun', 2]);
  assert.equal(staleRun(runs, false).action, 'none');
  // A dismissed approval turns a green run red.
  assert.equal(staleRun([run(2, 'success')], false).action, 'rerun');
  assert.equal(staleRun([run(2, 'success')], true).action, 'none');
  // A run still going may have read the PR before the review: wait for it.
  assert.equal(staleRun([run(2, 'failure'), run(4, null, { status: 'in_progress' })], true).action, 'wait');
  assert.equal(staleRun([run(4, null, { status: 'queued' })], false).action, 'wait');
});

test('Tom\'s approving review reruns the red Risk tier run, with nobody rerunning it', (t) => {
  const files = [{ filename: 'REVIEW.md', status: 'modified' }];
  const r = job(t, { files, reviews: approved, cmd: 'rerun', runs: [[run(40280097664, 'cancelled'), run(40280100259, 'failure')]] })();
  assert.equal(r.code, 0, r.out);
  assert.match(r.calls, /^gh api -X POST repos\/owner\/repo\/actions\/runs\/40280100259\/rerun$/m);
  assert.equal(r.calls.match(/-X POST/g).length, 1);
  assert.match(r.out, new RegExp(`^Risk tier on aaaaaaa: pass \\(approved: review by ${APPROVER} on aaaaaaa\\); run 40280100259 says failure, the verdict now is pass: rerun$`, 'm'));
});

test('a review that changes nothing reruns nothing; one mid-run waits for it to finish', (t) => {
  const files = [{ filename: 'REVIEW.md', status: 'modified' }];
  // A comment, not an approval: the red stands.
  let r = job(t, { files, reviews: [{ user: APPROVER, state: 'COMMENTED', commit_id: HEAD }], cmd: 'rerun', runs: [[run(7, 'failure')]] })();
  assert.equal(r.code, 0, r.out);
  assert.doesNotMatch(r.calls, /-X/);
  assert.match(r.out, /run 7 already says failure$/m);
  // An ordinary PR passed already.
  r = job(t, { files: [{ filename: 'src/server.mjs', status: 'modified' }], cmd: 'rerun', runs: [[run(7, 'success')]] })();
  assert.equal(r.code, 0, r.out);
  assert.doesNotMatch(r.calls, /-X/);
  // A run that read the PR before Tom's review landed, and ends red.
  r = job(t, { files, reviews: approved, cmd: 'rerun', runs: [[run(8, null, { status: 'in_progress' })], [run(8, null, { status: 'in_progress' })], [run(8, 'failure')]] })();
  assert.equal(r.code, 0, r.out);
  assert.match(r.calls, /^gh api -X POST repos\/owner\/repo\/actions\/runs\/8\/rerun$/m);
  assert.equal(r.calls.match(/actions\/workflows/g).length, 3);
  // One that never finishes: no rerun, and the job says so.
  r = job(t, { files, reviews: approved, cmd: 'rerun', runs: [[run(9, null, { status: 'queued' })]] })();
  assert.equal(r.code, 1, r.out);
  assert.doesNotMatch(r.calls, /-X/);
  assert.match(r.out, /run 9 is queued still; not rerun$/m);
  // A merged PR has nothing left to gate.
  r = job(t, { files, reviews: approved, state: 'closed', cmd: 'rerun', runs: [[run(7, 'failure')]] })();
  assert.equal(r.code, 0, r.out);
  assert.doesNotMatch(r.calls, /actions|-X/);
  assert.equal(job(t, { files, cmd: 'rerun' })('--pr', 'x').code, 2);
});

test('the job fails closed when GitHub can\'t be read, and on bad arguments', (t) => {
  const r0 = job(t, { files: [] });
  // The stub answers only for #12; for #13 it fails as gh does on a 404.
  const r = r0('--pr', '13');
  assert.equal(r.code, 2, r.out);
  assert.match(r.out, /^risk-tier: Command failed: gh api repos\/owner\/repo\/pulls\/13/m);
  assert.equal(r0('--pr').code, 2);
  assert.equal(r0('--pr', 'twelve').code, 2);
  assert.equal(spawnSync('node', [SCRIPT, 'nonsense'], { encoding: 'utf8' }).status, 2);
});

test('match reads git\'s name-status on stdin and prints path and reason, from a copy reached through a symlink', (t) => {
  const { dir, script } = fixture(t);
  const out = execFileSync('node', [script, 'match'], { input: 'M\tsrc/server.mjs\nD\ttest/a.test.mjs\nM\tschema.sql\n', encoding: 'utf8' });
  assert.deepEqual(out.trim().split('\n').map((l) => l.split('\t')[0]), ['test/a.test.mjs (deleted)', 'schema.sql']);
  assert.equal(execFileSync('node', [script, 'match'], { input: 'M\tsrc/server.mjs\n', encoding: 'utf8' }), '');
  // A land script may run main's copy through a link to its checkout.
  symlinkSync(path.join(dir, 'repo'), path.join(dir, 'link'));
  const linked = execFileSync('node', [path.join(dir, 'link/scripts/risk-tier.mjs'), 'match'], { input: 'M\tschema.sql\n', encoding: 'utf8' });
  assert.match(linked, /^schema\.sql\t/);
});
