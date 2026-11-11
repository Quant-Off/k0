'use strict';

// CLA 서명 확인 봇, actions/github-script에서 실행됨
// 풀 리퀘스트의 코드는 읽지도 실행하지도 않음, 이 파일과 CLA.md는 기본 브랜치나 PR base의 것

const fs = require('node:fs');

const CONFIG = Object.freeze({
  claPath: 'CLA.md',
  branch: 'cla-signatures',
  label: 'cla: trivial',
  // 워크플로 작업 이름과 헷갈리지 않도록 상태 이름을 따로 둠, 필수 검사로 지정할 것은 이 상태
  context: 'CLA signatures',
  marker: '<!-- k0-cla-bot -->',
  recheck: 'recheck',
  // 저작권자 계정은 서명 대상이 아님, 로그인 이름은 바뀔 수 있으므로 숫자 ID로 고정
  exemptIds: Object.freeze([53819958]),
  // pulls.listCommits가 돌려주는 커밋 수의 상한
  maxCommits: 250,
  writeAttempts: 5,
});

function parseCla(text) {
  const version = /^Version (\d+(?:\.\d+)+)\b/m.exec(text);
  if (!version) throw new Error('CLA.md: version line not found');
  const section = /^## [^\n]*How to sign[^\n]*\n([\s\S]*?)(?=^## |(?![\s\S]))/m.exec(text);
  if (!section) throw new Error('CLA.md: signing section not found');
  const fence = /^```[^\n]*\n([^\n]+)\n```/m.exec(section[1]);
  if (!fence) throw new Error('CLA.md: signing sentence not found');
  const sentence = fence[1].trim();
  if (!sentence.includes(`version ${version[1]}`)) {
    throw new Error('CLA.md: signing sentence does not name the current version');
  }
  return { version: version[1], sentence };
}

function normalize(body) {
  return String(body ?? '').replace(/\r\n?/g, '\n').trim();
}

function recordPath(version) {
  return `signatures/cla-v${version}.json`;
}

function emptyRecord(version) {
  return { agreement: CONFIG.claPath, version, signatures: [] };
}

function validateRecord(data, version) {
  if (!data || typeof data !== 'object' || data.version !== version || !Array.isArray(data.signatures)) {
    throw new Error('signature record is malformed');
  }
  for (const s of data.signatures) {
    if (!s || !Number.isSafeInteger(s.account_id) || typeof s.login !== 'string' || typeof s.comment_url !== 'string') {
      throw new Error('signature record entry is malformed');
    }
  }
  return data;
}

async function loadRecord(github, repo, version) {
  try {
    await github.rest.git.getRef({ ...repo, ref: `heads/${CONFIG.branch}` });
  } catch (e) {
    if (e.status === 404) return { branch: false, sha: null, data: emptyRecord(version) };
    throw e;
  }
  let res;
  try {
    res = await github.rest.repos.getContent({ ...repo, path: recordPath(version), ref: CONFIG.branch });
  } catch (e) {
    if (e.status === 404) return { branch: true, sha: null, data: emptyRecord(version) };
    throw e;
  }
  if (Array.isArray(res.data) || res.data.type !== 'file' || typeof res.data.content !== 'string') {
    throw new Error('signature record is not a readable file');
  }
  const text = Buffer.from(res.data.content, 'base64').toString('utf8');
  return { branch: true, sha: res.data.sha, data: validateRecord(JSON.parse(text), version) };
}

async function addSignature(github, repo, version, entry) {
  const path = recordPath(version);
  const message = `CLA 서명 기록 추가 ${entry.login}`;
  let last = null;
  for (let attempt = 0; attempt < CONFIG.writeAttempts; attempt++) {
    const rec = await loadRecord(github, repo, version);
    if (rec.data.signatures.some((s) => s.account_id === entry.account_id)) return false;
    rec.data.signatures.push(entry);
    const content = `${JSON.stringify(rec.data, null, 2)}\n`;
    try {
      if (!rec.branch) {
        // 기록 브랜치는 코드와 이력을 공유하지 않는 고아 브랜치로 만듦
        const tree = await github.rest.git.createTree({
          ...repo,
          tree: [{ path, mode: '100644', type: 'blob', content }],
        });
        const commit = await github.rest.git.createCommit({ ...repo, message, tree: tree.data.sha, parents: [] });
        await github.rest.git.createRef({ ...repo, ref: `refs/heads/${CONFIG.branch}`, sha: commit.data.sha });
      } else {
        await github.rest.repos.createOrUpdateFileContents({
          ...repo,
          path,
          branch: CONFIG.branch,
          message,
          content: Buffer.from(content, 'utf8').toString('base64'),
          ...(rec.sha ? { sha: rec.sha } : {}),
        });
      }
      return true;
    } catch (e) {
      // 다른 실행이 먼저 기록했을 수 있으므로 다시 읽고 재시도
      if (e.status !== 409 && e.status !== 422) throw e;
      last = e;
    }
  }
  throw new Error(`could not record the signature after ${CONFIG.writeAttempts} attempts: ${last && last.message}`);
}

async function requiredSigners(github, repo, pr) {
  const signers = new Map();
  const add = (user) => {
    if (!user || user.type === 'Bot' || !Number.isSafeInteger(user.id) || CONFIG.exemptIds.includes(user.id)) return;
    signers.set(user.id, user.login);
  };
  // 커밋 작성자 정보는 위조할 수 있으므로 풀 리퀘스트를 연 계정은 항상 서명 대상
  add(pr.user);
  const commits = await github.paginate(github.rest.pulls.listCommits, {
    ...repo,
    pull_number: pr.number,
    per_page: 100,
  });
  const unlinked = [];
  for (const c of commits) {
    if (c.author && Number.isSafeInteger(c.author.id)) add(c.author);
    else unlinked.push(String(c.sha).slice(0, 7));
  }
  return { signers, unlinked, tooMany: commits.length >= CONFIG.maxCommits };
}

function hasTrivialLabel(pr) {
  return (pr.labels || []).some((l) => l && l.name === CONFIG.label);
}

function links(env) {
  const base = `https://github.com/${env.repo.owner}/${env.repo.repo}/blob/${env.defaultBranch}`;
  return { cla: `${base}/CLA.md`, contributingKr: `${base}/CONTRIBUTING_KR.md` };
}

async function setStatus(env, sha, state, description) {
  await env.github.rest.repos.createCommitStatus({
    ...env.repo,
    sha,
    state,
    context: CONFIG.context,
    description: description.slice(0, 140),
    target_url: links(env).cla,
  });
}

async function listComments(env, number, extra) {
  const comments = await env.github.paginate(env.github.rest.issues.listComments, {
    ...env.repo,
    issue_number: number,
    per_page: 100,
  });
  // 방금 달린 댓글이 목록에 아직 없을 수 있으므로 이벤트의 댓글을 합침
  if (extra && !comments.some((c) => c.id === extra.id)) comments.push(extra);
  return comments;
}

function isBotComment(c) {
  // 표식만으로는 믿지 않음, 이 워크플로의 봇 계정이 쓴 댓글만 봇 댓글로 봄
  return (
    c.user &&
    c.user.type === 'Bot' &&
    c.user.login === 'github-actions[bot]' &&
    typeof c.body === 'string' &&
    c.body.startsWith(CONFIG.marker)
  );
}

function isSignature(env, c) {
  // 수정된 댓글은 서명으로 보지 않음, 서명은 새 댓글로만 받음
  return (
    c.user &&
    c.user.type !== 'Bot' &&
    Number.isSafeInteger(c.user.id) &&
    c.created_at === c.updated_at &&
    normalize(c.body) === env.cla.sentence
  );
}

async function upsertComment(env, number, comments, body, createIfMissing) {
  const existing = comments.find(isBotComment);
  if (existing) {
    if (existing.body !== body) {
      await env.github.rest.issues.updateComment({ ...env.repo, comment_id: existing.id, body });
    }
  } else if (createIfMissing) {
    await env.github.rest.issues.createComment({ ...env.repo, issue_number: number, body });
  }
}

function header() {
  return `${CONFIG.marker}\n### Contributor License Agreement\n\n`;
}

function failureBody(env, pr, { missing, unlinked, tooMany, notes }) {
  const l = links(env);
  const parts = [header()];
  parts.push(`This pull request needs a signature on the [K0 CLA version ${env.cla.version}](${l.cla}) before it can be merged.\n\n`);
  for (const n of notes) parts.push(`${n}\n\n`);
  if (missing.size) {
    // 커밋 작성자 계정은 위조될 수 있으므로 멘션은 풀 리퀘스트를 연 계정에만 함
    const names = [...missing].map(([id, login]) => (pr.user && id === pr.user.id ? `@${login}` : `\`${login}\``));
    parts.push(`Still to sign: ${names.join(', ')}\n\n`);
    parts.push('To sign, post a new comment on this pull request that contains only this sentence:\n\n');
    parts.push(`\`\`\`\n${env.cla.sentence}\n\`\`\`\n\n`);
  }
  if (unlinked.length) {
    parts.push(
      `These commits have an author email that is not linked to any GitHub account, so their author cannot be identified: ${unlinked
        .map((s) => `\`${s}\``)
        .join(', ')}. Link the email to the author's GitHub account, or rewrite the commits with a linked email, and push again.\n\n`,
    );
  }
  if (tooMany) {
    parts.push(`This pull request has ${CONFIG.maxCommits} commits or more, which is more than this check can read. Please split it.\n\n`);
  }
  parts.push(
    `If you already signed on another pull request, comment \`${CONFIG.recheck}\`. If every change here is trivial as described in section 12 of the CLA, a maintainer can mark the pull request instead.\n\n`,
  );
  parts.push(`한국어 안내는 [CONTRIBUTING_KR.md](${l.contributingKr})에 있습니다.\n`);
  return parts.join('');
}

function signedBody(env) {
  return `${header()}Everyone who contributed to this pull request has signed the K0 CLA version ${env.cla.version}.\n`;
}

function trivialBody() {
  return `${header()}A maintainer marked every change in this pull request as trivial under section 12 of the CLA, so no signature is needed.\n`;
}

function describe({ missing, unlinked, tooMany }) {
  if (tooMany) return 'Too many commits to check';
  const parts = [];
  if (missing.size) parts.push(`Signature needed from ${[...missing.values()].join(', ')}`);
  if (unlinked.length) parts.push(`${unlinked.length} commit(s) with an unknown author`);
  return parts.join('; ');
}

async function evaluate(env, pr, notes = [], extra = null) {
  const sha = pr.head.sha;
  const comments = await listComments(env, pr.number, extra);
  if (hasTrivialLabel(pr)) {
    await setStatus(env, sha, 'success', 'Trivial change under CLA section 12, marked by a maintainer');
    await upsertComment(env, pr.number, comments, trivialBody(), false);
    return 'trivial';
  }
  const { signers, unlinked, tooMany } = await requiredSigners(env.github, env.repo, pr);
  const record = await loadRecord(env.github, env.repo, env.cla.version);
  const signed = new Set(record.data.signatures.map((s) => s.account_id));
  const missing = new Map([...signers].filter(([id]) => !signed.has(id)));
  // 서명 댓글로 시작된 실행은 같은 동시 실행 그룹의 다음 이벤트에 밀려 취소될 수 있음
  // 그래서 기록은 매 확인 때 댓글 목록에서 함
  for (const c of comments) {
    if (missing.has(c.user && c.user.id) && isSignature(env, c)) {
      const added = await addSignature(env.github, env.repo, env.cla.version, {
        account_id: c.user.id,
        login: c.user.login,
        version: env.cla.version,
        comment_id: c.id,
        comment_url: c.html_url,
        pull_request: pr.number,
        signed_at: c.created_at,
      });
      env.core.info(added ? `recorded the signature of ${c.user.login}` : `${c.user.login} had already signed`);
      missing.delete(c.user.id);
    }
  }
  if (!tooMany && unlinked.length === 0 && missing.size === 0) {
    await setStatus(env, sha, 'success', `All contributors have signed CLA version ${env.cla.version}`);
    await upsertComment(env, pr.number, comments, signedBody(env), false);
    return 'signed';
  }
  const result = { missing, unlinked, tooMany, notes };
  await setStatus(env, sha, 'failure', describe(result));
  await upsertComment(env, pr.number, comments, failureBody(env, pr, result), true);
  return 'missing';
}

async function onPullRequest(env, payload) {
  const pr = { ...payload.pull_request, labels: [...(payload.pull_request.labels || [])] };
  const notes = [];
  if (payload.action === 'synchronize' && hasTrivialLabel(pr)) {
    // 사소한 변경 판정은 그때의 커밋에 대한 것, 새 커밋이 오면 다시 판정받아야 함
    try {
      await env.github.rest.issues.removeLabel({ ...env.repo, issue_number: pr.number, name: CONFIG.label });
    } catch (e) {
      if (e.status !== 404) throw e;
    }
    pr.labels = pr.labels.filter((l) => l && l.name !== CONFIG.label);
    notes.push(
      `New commits were pushed after a maintainer marked this pull request as trivial, so the \`${CONFIG.label}\` label was removed. A maintainer needs to look at it again.`,
    );
  }
  return evaluate(env, pr, notes);
}

async function onComment(env, payload, setHead) {
  const { action, issue, comment } = payload;
  if (action !== 'created' || !issue || !issue.pull_request || !comment || !comment.user) return 'ignored';
  // 어떤 댓글이든 다시 확인함, 이 실행이 같은 그룹에서 대기하던 서명 실행을 밀어냈을 수 있기 때문
  // 봇 댓글은 isSignature에서 서명으로 보지 않음
  const pr = (await env.github.rest.pulls.get({ ...env.repo, pull_number: issue.number })).data;
  if (pr.state !== 'open') return 'closed';
  setHead(pr.head.sha);
  return evaluate(env, pr, [], comment);
}

async function run({ github, context, core, claText }) {
  const text = claText ?? fs.readFileSync(CONFIG.claPath, 'utf8');
  const env = {
    github,
    core,
    repo: { owner: context.repo.owner, repo: context.repo.repo },
    cla: parseCla(text),
    defaultBranch: (context.payload.repository && context.payload.repository.default_branch) || 'main',
  };
  let head = null;
  try {
    if (context.eventName === 'pull_request_target') {
      head = context.payload.pull_request.head.sha;
      return await onPullRequest(env, context.payload);
    }
    if (context.eventName === 'issue_comment') {
      return await onComment(env, context.payload, (sha) => {
        head = sha;
      });
    }
    core.info(`ignored event ${context.eventName}`);
    return 'ignored';
  } catch (e) {
    // 확인에 실패하면 통과로 남기지 않음
    if (head) {
      try {
        await setStatus(env, head, 'error', 'The CLA check failed, see the workflow log');
      } catch (inner) {
        core.warning(`could not set the error status: ${inner.message}`);
      }
    }
    throw e;
  }
}

module.exports = { CONFIG, parseCla, normalize, recordPath, validateRecord, run };
