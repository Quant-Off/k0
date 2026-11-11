'use strict';

// cla.js 단위 테스트, GitHub API는 메모리 안의 가짜로 대신함
// 실행: node --test .github/scripts/cla.test.js

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { CONFIG, parseCla, normalize, validateRecord, run } = require('./cla.js');

const ROOT = path.join(__dirname, '..', '..');
const CLA_TEXT = fs.readFileSync(path.join(ROOT, 'CLA.md'), 'utf8');
const SENTENCE = parseCla(CLA_TEXT).sentence;
const MAINTAINER = { id: 53819958, login: 'maintainer', type: 'User' };
const ALICE = { id: 1001, login: 'alice', type: 'User' };
const BOB = { id: 1002, login: 'bob', type: 'User' };
const BOT = { id: 49699333, login: 'dependabot[bot]', type: 'Bot' };
const ACTIONS_BOT = { id: 41898282, login: 'github-actions[bot]', type: 'Bot' };

function httpError(status) {
  return Object.assign(new Error(`HTTP ${status}`), { status });
}

function fakeGithub({ pr, commits, record = null, conflicts = 0, comments = [] }) {
  const s = {
    pr,
    commits,
    branch: record !== null,
    file: record === null ? null : JSON.stringify(record),
    fileSha: record === null ? null : 'f0',
    writes: 0,
    conflicts,
    statuses: [],
    comments: comments.map((c) => ({ ...c })),
    created: 0,
    updated: 0,
    removedLabels: [],
    orphanParents: null,
    nextId: 9000,
  };
  let pendingTree = null;
  const github = {
    paginate: async (fn, params) => (await fn(params)).data,
    rest: {
      pulls: {
        get: async () => ({ data: s.pr }),
        listCommits: async () => ({ data: s.commits }),
      },
      repos: {
        createCommitStatus: async (p) => {
          s.statuses.push(p);
          return { data: {} };
        },
        getContent: async (p) => {
          assert.equal(p.ref, CONFIG.branch);
          if (!s.branch || s.file === null) throw httpError(404);
          return { data: { type: 'file', sha: s.fileSha, content: Buffer.from(s.file).toString('base64') } };
        },
        createOrUpdateFileContents: async (p) => {
          assert.equal(p.branch, CONFIG.branch);
          if (s.conflicts > 0) {
            s.conflicts -= 1;
            throw httpError(409);
          }
          if (s.file !== null && p.sha !== s.fileSha) throw httpError(409);
          s.file = Buffer.from(p.content, 'base64').toString('utf8');
          s.writes += 1;
          s.fileSha = `f${s.writes}`;
          return { data: {} };
        },
      },
      git: {
        getRef: async (p) => {
          assert.equal(p.ref, `heads/${CONFIG.branch}`);
          if (!s.branch) throw httpError(404);
          return { data: {} };
        },
        createTree: async (p) => {
          pendingTree = p.tree;
          return { data: { sha: 't1' } };
        },
        createCommit: async (p) => {
          s.orphanParents = p.parents;
          return { data: { sha: 'c1' } };
        },
        createRef: async (p) => {
          assert.equal(p.ref, `refs/heads/${CONFIG.branch}`);
          if (s.branch) throw httpError(422);
          s.branch = true;
          s.file = pendingTree[0].content;
          s.writes += 1;
          s.fileSha = `f${s.writes}`;
          return { data: {} };
        },
      },
      issues: {
        listComments: async () => ({ data: s.comments }),
        createComment: async (p) => {
          s.created += 1;
          const c = { id: (s.nextId += 1), body: p.body, user: ACTIONS_BOT };
          s.comments.push(c);
          return { data: c };
        },
        updateComment: async (p) => {
          s.updated += 1;
          const c = s.comments.find((x) => x.id === p.comment_id);
          assert.ok(c, 'updated comment must exist');
          c.body = p.body;
          return { data: c };
        },
        removeLabel: async (p) => {
          s.removedLabels.push(p.name);
          return { data: [] };
        },
      },
    },
  };
  return { github, s };
}

const core = { info() {}, warning() {} };

function makePr(user, { labels = [], state = 'open', number = 7 } = {}) {
  return { number, state, user, labels, head: { sha: 'headsha' } };
}

function commit(author, sha = 'abcdef1234') {
  return { sha, author };
}

function prContext(pr, action = 'opened') {
  return {
    eventName: 'pull_request_target',
    repo: { owner: 'Quant-Off', repo: 'k0' },
    payload: { action, pull_request: pr, repository: { default_branch: 'main' } },
  };
}

function commentContext(user, body, { number = 7, id = 555 } = {}) {
  return {
    eventName: 'issue_comment',
    repo: { owner: 'Quant-Off', repo: 'k0' },
    payload: {
      action: 'created',
      issue: { number, pull_request: {} },
      comment: {
        id,
        user,
        body,
        html_url: `https://github.com/Quant-Off/k0/pull/${number}#issuecomment-${id}`,
        created_at: '2026-11-11T00:00:00Z',
        updated_at: '2026-11-11T00:00:00Z',
      },
      repository: { default_branch: 'main' },
    },
  };
}

function record(signers) {
  return {
    agreement: 'CLA.md',
    version: '1.0',
    signatures: signers.map((u) => ({
      account_id: u.id,
      login: u.login,
      version: '1.0',
      comment_id: 1,
      comment_url: 'https://example.invalid/c',
      pull_request: 1,
      signed_at: '2026-01-01T00:00:00Z',
    })),
  };
}

const lastStatus = (s) => s.statuses[s.statuses.length - 1];
const botComments = (s) => s.comments.filter((c) => c.user === ACTIONS_BOT);

test('the sentence and version come from CLA.md and match CONTRIBUTING', () => {
  const cla = parseCla(CLA_TEXT);
  assert.equal(cla.version, '1.0');
  for (const doc of ['CONTRIBUTING.md', 'CONTRIBUTING_KR.md']) {
    assert.ok(fs.readFileSync(path.join(ROOT, doc), 'utf8').includes(cla.sentence), doc);
  }
});

test('a CLA without a version, a signing block or a matching version is rejected', () => {
  assert.throws(() => parseCla('# CLA\n## 11. How to sign\n```\nI agree to version 1.0\n```\n'));
  assert.throws(() => parseCla('Version 1.0\n## 11. How to sign\nno block\n## 12. Next\n'));
  assert.throws(() => parseCla('Version 2.0\n## 11. How to sign\n```\nI agree to version 1.0\n```\n'));
  assert.throws(() => parseCla('Version 1.0\n## 11. How to sign\n```\nline one version 1.0\nline two\n```\n'));
});

test('only whitespace and line endings are normalized', () => {
  assert.equal(normalize(`  ${SENTENCE}\r\n`), SENTENCE);
  assert.notEqual(normalize(`${SENTENCE}!`), SENTENCE);
  assert.notEqual(normalize(`> ${SENTENCE}`), SENTENCE);
});

test('a malformed record fails closed', () => {
  assert.throws(() => validateRecord({ version: '1.0', signatures: [{ account_id: '1', login: 'x', comment_url: 'u' }] }, '1.0'));
  assert.throws(() => validateRecord({ version: '0.9', signatures: [] }, '1.0'));
  assert.throws(() => validateRecord(null, '1.0'));
});

test('an unsigned contributor gets a failing status and one instruction comment', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)] });
  assert.equal(await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT }), 'missing');
  assert.equal(lastStatus(s).state, 'failure');
  assert.equal(lastStatus(s).context, 'CLA signatures');
  assert.equal(lastStatus(s).sha, 'headsha');
  const [c] = botComments(s);
  assert.ok(c.body.startsWith(CONFIG.marker));
  assert.ok(c.body.includes('@alice'));
  assert.ok(c.body.includes(SENTENCE));
  await run({ github, context: prContext(s.pr, 'synchronize'), core, claText: CLA_TEXT });
  assert.equal(s.created, 1, 'the same comment is reused');
});

test('signing creates the orphan record branch with the fields the CLA promises', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)] });
  await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT });
  assert.equal(await run({ github, context: commentContext(ALICE, SENTENCE), core, claText: CLA_TEXT }), 'signed');
  assert.deepEqual(s.orphanParents, []);
  const data = JSON.parse(s.file);
  assert.equal(data.version, '1.0');
  assert.deepEqual(data.signatures, [
    {
      account_id: 1001,
      login: 'alice',
      version: '1.0',
      comment_id: 555,
      comment_url: 'https://github.com/Quant-Off/k0/pull/7#issuecomment-555',
      pull_request: 7,
      signed_at: '2026-11-11T00:00:00Z',
    },
  ]);
  assert.equal(lastStatus(s).state, 'success');
  assert.equal(botComments(s).length, 1);
  assert.ok(botComments(s)[0].body.includes('has signed'));
});

test('a second signature appends to the existing record and survives a write conflict', async () => {
  const { github, s } = fakeGithub({
    pr: makePr(BOB),
    commits: [commit(BOB)],
    record: record([ALICE]),
    conflicts: 2,
  });
  assert.equal(await run({ github, context: commentContext(BOB, SENTENCE), core, claText: CLA_TEXT }), 'signed');
  assert.deepEqual(
    JSON.parse(s.file).signatures.map((x) => x.login),
    ['alice', 'bob'],
  );
});

test('signing twice records once', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)], record: record([ALICE]) });
  await run({ github, context: commentContext(ALICE, SENTENCE), core, claText: CLA_TEXT });
  assert.equal(s.writes, 0);
  assert.equal(JSON.parse(s.file).signatures.length, 1);
});

test('a near miss of the sentence is not a signature', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)] });
  const near = SENTENCE.replace('version 1.0', 'version 1.1');
  assert.equal(await run({ github, context: commentContext(ALICE, near), core, claText: CLA_TEXT }), 'missing');
  assert.equal(s.branch, false);
});

test('someone who did not contribute to the pull request is not recorded', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)] });
  assert.equal(await run({ github, context: commentContext(BOB, SENTENCE), core, claText: CLA_TEXT }), 'missing');
  assert.equal(s.branch, false);
});

test('every commit author has to sign, not only the pull request author', async () => {
  const { github, s } = fakeGithub({
    pr: makePr(ALICE),
    commits: [commit(ALICE, 'aaaaaaa1'), commit(BOB, 'bbbbbbb2')],
    record: record([ALICE]),
  });
  await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT });
  assert.equal(lastStatus(s).state, 'failure');
  const body = botComments(s)[0].body;
  assert.ok(body.includes('`bob`'), 'other commit authors are listed');
  assert.ok(!body.includes('@bob'), 'commit authors can be forged, so they are not mentioned');
  assert.ok(!body.includes('alice'));
});

test('the pull request author must sign even if every commit claims the maintainer as author', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(MAINTAINER)] });
  await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT });
  assert.equal(lastStatus(s).state, 'failure');
  assert.ok(botComments(s)[0].body.includes('@alice'));
});

test('the maintainer and bots do not need to sign', async () => {
  for (const user of [MAINTAINER, BOT]) {
    const { github, s } = fakeGithub({ pr: makePr(user), commits: [commit(user)] });
    assert.equal(await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT }), 'signed');
    assert.equal(lastStatus(s).state, 'success');
    assert.equal(s.created, 0, 'no comment when nothing is missing');
  }
});

test('a commit whose author is not linked to an account fails the check', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(null, '1234567890')], record: record([ALICE]) });
  await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT });
  assert.equal(lastStatus(s).state, 'failure');
  assert.ok(botComments(s)[0].body.includes('`1234567`'));
});

test('a pull request with too many commits fails closed', async () => {
  const many = Array.from({ length: CONFIG.maxCommits }, (_, i) => commit(ALICE, `c${i}xxxxxx`));
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: many, record: record([ALICE]) });
  await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT });
  assert.equal(lastStatus(s).state, 'failure');
  assert.equal(lastStatus(s).description, 'Too many commits to check');
});

test('the trivial label passes the check and is removed again by new commits', async () => {
  const label = [{ name: CONFIG.label }];
  const { github, s } = fakeGithub({ pr: makePr(ALICE, { labels: label }), commits: [commit(ALICE)] });
  assert.equal(await run({ github, context: prContext(s.pr, 'labeled'), core, claText: CLA_TEXT }), 'trivial');
  assert.equal(lastStatus(s).state, 'success');
  assert.equal(await run({ github, context: prContext(s.pr, 'synchronize'), core, claText: CLA_TEXT }), 'missing');
  assert.deepEqual(s.removedLabels, [CONFIG.label]);
  assert.equal(lastStatus(s).state, 'failure');
  assert.ok(botComments(s)[0].body.includes('label was removed'));
});

function userComment(user, body, { id = 77, edited = false } = {}) {
  return {
    id,
    user,
    body,
    html_url: `https://github.com/Quant-Off/k0/pull/7#issuecomment-${id}`,
    created_at: '2026-11-10T00:00:00Z',
    updated_at: edited ? '2026-11-10T01:00:00Z' : '2026-11-10T00:00:00Z',
  };
}

test('a signature is recorded even when the run its comment started was cancelled', async () => {
  const { github, s } = fakeGithub({
    pr: makePr(ALICE),
    commits: [commit(ALICE)],
    comments: [userComment(ALICE, SENTENCE)],
  });
  assert.equal(await run({ github, context: prContext(s.pr, 'labeled'), core, claText: CLA_TEXT }), 'signed');
  const [entry] = JSON.parse(s.file).signatures;
  assert.equal(entry.account_id, 1001);
  assert.equal(entry.comment_id, 77);
  assert.equal(entry.signed_at, '2026-11-10T00:00:00Z');
});

test('the sentence with other text around it is not a signature', async () => {
  const wrapped = `Sure!\n${SENTENCE}`;
  const { github, s } = fakeGithub({
    pr: makePr(ALICE),
    commits: [commit(ALICE)],
    comments: [userComment(ALICE, wrapped)],
  });
  assert.equal(await run({ github, context: commentContext(ALICE, wrapped), core, claText: CLA_TEXT }), 'missing');
  assert.equal(await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT }), 'missing');
  assert.equal(s.branch, false);
});

test('an edited comment is not a signature', async () => {
  const { github, s } = fakeGithub({
    pr: makePr(ALICE),
    commits: [commit(ALICE)],
    comments: [userComment(ALICE, SENTENCE, { edited: true })],
  });
  assert.equal(await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT }), 'missing');
  assert.equal(s.branch, false);
});

test('recheck re-evaluates after a signature made elsewhere', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)], record: record([ALICE]) });
  assert.equal(await run({ github, context: commentContext(ALICE, 'recheck'), core, claText: CLA_TEXT }), 'signed');
  assert.equal(lastStatus(s).state, 'success');
});

test('edit events and comments on issues are ignored', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)] });
  const edited = commentContext(ALICE, SENTENCE);
  edited.payload.action = 'edited';
  const onIssue = commentContext(ALICE, SENTENCE);
  delete onIssue.payload.issue.pull_request;
  for (const ctx of [edited, onIssue]) {
    assert.equal(await run({ github, context: ctx, core, claText: CLA_TEXT }), 'ignored');
  }
  assert.equal(s.statuses.length, 0);
});

test('a comment by a bot account is never a signature', async () => {
  const { github, s } = fakeGithub({ pr: makePr(BOT), commits: [commit(ALICE)] });
  assert.equal(await run({ github, context: commentContext(BOT, SENTENCE), core, claText: CLA_TEXT }), 'missing');
  assert.equal(s.branch, false);
});

test('any later comment records a signature whose own run was displaced', async () => {
  const { github, s } = fakeGithub({
    pr: makePr(ALICE),
    commits: [commit(ALICE)],
    comments: [userComment(ALICE, SENTENCE)],
  });
  const unrelated = commentContext(BOB, 'Looks good to me', { id: 556 });
  assert.equal(await run({ github, context: unrelated, core, claText: CLA_TEXT }), 'signed');
  assert.equal(JSON.parse(s.file).signatures[0].comment_id, 77);
});

test('a user comment that copies the marker is never edited', async () => {
  const forged = { id: 1, body: `${CONFIG.marker}\nfake`, user: ALICE };
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)], comments: [forged] });
  await run({ github, context: prContext(s.pr), core, claText: CLA_TEXT });
  assert.equal(s.comments[0].body, `${CONFIG.marker}\nfake`);
  assert.equal(s.created, 1);
});

test('a record for another version never counts as a signature', async () => {
  const stale = { ...record([ALICE]), version: '0.9' };
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)], record: stale });
  await assert.rejects(run({ github, context: prContext(s.pr), core, claText: CLA_TEXT }));
  assert.equal(lastStatus(s).state, 'error');
});

test('a corrupt record sets an error status and fails the run', async () => {
  const { github, s } = fakeGithub({ pr: makePr(ALICE), commits: [commit(ALICE)], record: { version: '1.0' } });
  await assert.rejects(run({ github, context: prContext(s.pr), core, claText: CLA_TEXT }));
  assert.equal(lastStatus(s).state, 'error');
});
