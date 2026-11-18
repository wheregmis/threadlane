// Remove only merge entries proven redundant with an implementation entry.
async function cleanNotes(text, {github, owner, repo}) {
  const entries = [];
  let section = '';
  for (const line of text.split('\n')) {
    if (/^### /.test(line)) section = line;
    const match = line.match(/^\* (.+) \(\[([0-9a-f]+)\]\(https:\/\/github\.com\/([^/]+\/[^/]+)\/commit\/([0-9a-f]{40})\)\)$/);
    if (match && match[3] === owner + '/' + repo) entries.push({line, key: section + '\n' + match[1], sha: match[4]});
  }
  const removed = new Set();
  const cache = new Map();
  async function commit(sha) {
    if (!cache.has(sha)) cache.set(sha, (await github.rest.repos.getCommit({owner, repo, ref: sha})).data);
    return cache.get(sha);
  }
  for (const entry of entries) {
    const peers = entries.filter(other => other.key === entry.key && other.sha !== entry.sha);
    if (!peers.length) continue;
    const merge = await commit(entry.sha);
    if (merge.parents.length !== 2 || !/^Merge pull request #\d+ /.test(merge.commit.message)) continue;
    for (const peer of peers) {
      const original = await commit(peer.sha);
      if (original.parents.length > 1) continue;
      const subject = original.commit.message.split('\n')[0];
      if (!/^[a-z]+(?:\([^)]*\))?!?: /.test(subject) || !merge.commit.message.split('\n').includes(subject)) continue;
      const comparison = (await github.rest.repos.compareCommits({
        owner, repo, base: peer.sha, head: merge.parents[1].sha
      })).data;
      if (comparison.status === 'ahead' || comparison.status === 'identical') {
        removed.add(entry.line);
        break;
      }
    }
  }
  return text.split('\n').filter(line => !removed.has(line)).join('\n');
}

module.exports = async ({github, context, core}) => {
  const {owner, repo} = context.repo;
  for (const candidate of JSON.parse(process.env.RELEASE_PRS || '[]')) {
    const pr = (await github.rest.pulls.get({owner, repo, pull_number: candidate.number})).data;
    if (pr.state !== 'open' || pr.head.repo.full_name !== owner + '/' + repo) continue;
    // Scope cleanup to the current release; never rewrite past releases.
    const file = (await github.rest.repos.getContent({owner, repo, path: 'CHANGELOG.md', ref: pr.head.sha})).data;
    const changelog = Buffer.from(file.content, 'base64').toString('utf8');
    const start = changelog.search(/^## /m);
    if (start < 0) throw new Error('Missing current changelog section');
    const next = changelog.slice(start + 1).search(/^## /m);
    const end = next < 0 ? changelog.length : start + 1 + next;
    const current = changelog.slice(start, end);
    const cleaned = await cleanNotes(current, {github, owner, repo});
    const body = pr.body || '';
    if (!body.includes(current.trim())) throw new Error('PR body and current changelog differ');
    if (cleaned === current) continue;
    // Refuse to update a release branch that changed during validation.
    const fresh = (await github.rest.pulls.get({owner, repo, pull_number: pr.number})).data;
    if (fresh.head.sha !== pr.head.sha || fresh.state !== 'open') throw new Error('Release PR changed during cleanup');
    await github.rest.repos.createOrUpdateFileContents({
      owner, repo, path: 'CHANGELOG.md', branch: pr.head.ref, sha: file.sha,
      message: 'chore: remove verified duplicate merge release notes',
      content: Buffer.from(changelog.slice(0, start) + cleaned + changelog.slice(end)).toString('base64')
    });
    await github.rest.pulls.update({
      owner, repo, pull_number: pr.number, body: body.replace(current.trim(), cleaned.trim())
    });
    core.info('Removed verified duplicate merge entries from release PR #' + pr.number);
  }
};
module.exports.cleanNotes = cleanNotes;
