// managed-by: clud
export const meta = {
  name: 'grind-run',
  description: 'Engine behind clud /grind (start it with /grind): plan, write, review, integrate and land a list of goals, in parallel worktrees or sequentially in the local checkout, with per-role caps.',
  whenToUse: 'Started by the /grind router skill, which gathers the goals, mode, models and CI choice first. Do not start it directly.',
  phases: [
    { title: 'Plan', detail: 'grind-planner: split each goal into disjoint file tasks' },
    { title: 'Work', detail: 'grind-worker: read/write files only' },
    { title: 'Review', detail: 'grind-reviewer: read/edit only' },
    { title: 'Integrate', detail: 'grind-integrator: one at a time; rebase, lint, build, test, push' },
    { title: 'Land', detail: 'grind-lander: pr_merge_watch, admin merge or hand back for fixes' },
  ],
}

// args: {
//   repo: '/abs/repo', main: 'main', mode: 'parallel' | 'sequential',
//   goals: [{ id, title, brief }], ci: false, lane?: 'job-id',
//   models?: { planner, worker, reviewer, integrator },   // omitted = session model
//   maxAgents?: 4, maxFixRounds?: 10,
// }
if (!args || !args.repo || !Array.isArray(args.goals) || !args.goals.length) {
  throw new Error('grind needs args {repo, mode, goals:[{id,title,brief}]}; start it with /grind')
}
if (args.mode !== 'parallel' && args.mode !== 'sequential') {
  throw new Error(`grind mode must be "parallel" or "sequential" (got ${JSON.stringify(args.mode)}); cron mode is driven by /grind-cron`)
}
const REPO = args.repo
const MAIN = args.main || 'main'
const PARALLEL = args.mode === 'parallel'
const CI = !!args.ci
const MAX_AGENTS = Math.max(1, Math.floor(args.maxAgents ?? 4))
const MAX_FIX = Math.max(0, Math.floor(args.maxFixRounds ?? 10))
const MODELS = args.models || {}

// Model per role; the lander shares the integrator's choice.
const opts = (role, label, phase, schema) => {
  const o = { label, phase, schema, agentType: `grind-${role}` }
  const m = MODELS[role === 'lander' ? 'integrator' : role]
  if (m) o.model = m
  return o
}

// Counting semaphore for plan/work/review agents, and a mutex for the
// integrator: builds never overlap, so caches stay warm and the CPU stays sane.
const semaphore = (n) => {
  let free = n
  const queue = []
  return (fn) => new Promise((resolve, reject) => {
    const run = () => {
      free--
      Promise.resolve().then(fn).then(resolve, reject).finally(() => {
        free++
        if (queue.length) queue.shift()()
      })
    }
    free > 0 ? run() : queue.push(run)
  })
}
const light = semaphore(MAX_AGENTS)
const exclusive = semaphore(1)

const PLAN = {
  type: 'object', required: ['checkout', 'branch', 'depends_on', 'tasks', 'verify'],
  properties: {
    checkout: { type: 'string', description: 'absolute path the goal is worked in (its worktree, or the repo in sequential mode)' },
    branch: { type: 'string' },
    depends_on: { type: 'array', items: { type: 'string' }, description: 'ids of other goals this one must land after; [] means isolated' },
    verify: { type: 'string', description: 'exact lint/build/test commands for the integrator, newline separated' },
    tasks: { type: 'array', items: { type: 'object', required: ['id', 'files', 'instructions'], properties: {
      id: { type: 'string' }, files: { type: 'array', items: { type: 'string' } }, instructions: { type: 'string' } } } },
  },
}
const WORK = { type: 'object', required: ['files_touched', 'summary'], properties: {
  files_touched: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' }, blocked: { type: 'string' } } }
const REVIEW = { type: 'object', required: ['approved', 'summary'], properties: {
  approved: { type: 'boolean' }, summary: { type: 'string' }, fixes_applied: { type: 'array', items: { type: 'string' } } } }
const INTEG = { type: 'object', required: ['pushed', 'summary'], properties: {
  pushed: { type: 'boolean' }, pr_url: { type: 'string' }, summary: { type: 'string' }, failure_log: { type: 'string' } } }
const LAND = { type: 'object', required: ['status', 'summary'], properties: {
  status: { type: 'string', enum: ['merged', 'needs_fix', 'gave_up'] }, summary: { type: 'string' }, failure_log: { type: 'string' } } }

const ctx = (g) => `Repo: ${REPO} (default branch ${MAIN}). Mode: ${args.mode}. Local CI (act): ${CI ? `on, lane ${args.lane || '(pick from ci.yml)'}` : 'off'}.\nGoal ${g.id}: ${g.title}\n${g.brief}`
const others = (g) => args.goals.filter(o => o.id !== g.id).map(o => `${o.id}: ${o.title}`).join('\n') || '(none)'

const plan = (g) => light(() => agent(
  `Invoke the /grind-plan skill and follow it.\n\n${ctx(g)}\n\nOther goals in this run (for depends_on):\n${others(g)}`,
  opts('planner', `plan:${g.id}`, 'Plan', PLAN)))

const work = (p, g) => (PARALLEL
  ? parallel(p.tasks.map(t => () => light(() => runTask(p, g, t))))
  : p.tasks.reduce((acc, t) => acc.then(rs => runTask(p, g, t).then(r => [...rs, r])), Promise.resolve([])))
  .then(rs => ({ p, results: rs.filter(Boolean) }))
const runTask = (p, g, t) => agent(
  `Invoke the /grind-work skill and follow it.\n\n${ctx(g)}\n\nCheckout: ${p.checkout}\nTask ${t.id}. Files you own: ${t.files.join(', ')}\n\n${t.instructions}`,
  opts('worker', `work:${g.id}/${t.id}`, 'Work', WORK))

const review = ({ p, results }, g) => light(() => agent(
  `Invoke the /grind-review skill and follow it.\n\n${ctx(g)}\n\nCheckout: ${p.checkout}\nWorker reports:\n${JSON.stringify(results, null, 1)}`,
  opts('reviewer', `review:${g.id}`, 'Review', REVIEW))).then(r => ({ p, review: r }))

const integrate = (p, g, note) => exclusive(() => agent(
  `Invoke the /grind-integrate skill and follow it.\n\n${ctx(g)}\n\nCheckout: ${p.checkout}\nBranch: ${p.branch}\n` +
  `Base: origin/${MAIN}${p.depends_on.length ? ` (it already contains ${p.depends_on.join(', ')}, which landed first)` : ''}\n` +
  `Verify commands:\n${p.verify}\n\n${note}`,
  opts('integrator', `integrate:${g.id}`, 'Integrate', INTEG)))

const land = (p, g, pr, round) => agent(
  `Invoke the /grind-land skill and follow it.\n\n${ctx(g)}\n\nPR: ${pr}\nBranch: ${p.branch}\nFix rounds used: ${round} of ${MAX_FIX}.`,
  opts('lander', `land:${g.id}#${round}`, 'Land', LAND))

// Dependents wait until what they depend on has merged, then rebase onto the
// new origin/main; isolated goals go straight to origin/main.
//
// A goal may depend only on goals listed *before* it. That rules out cycles
// and self-dependencies, and in sequential mode it means every dependency has
// already settled, so no wait can hang.
const landed = {}
const settle = {}
const order = {}
args.goals.forEach((g, i) => {
  order[g.id] = i
  landed[g.id] = new Promise(r => { settle[g.id] = r })
})
const earlierDeps = (p, g) => {
  const deps = (p.depends_on || []).map(String)
  const kept = deps.filter(d => d in order && order[d] < order[g.id])
  const dropped = deps.filter(d => !kept.includes(d))
  if (dropped.length) log(`goal ${g.id}: ignoring depends_on ${dropped.join(', ')} (unknown, itself, or not listed earlier)`)
  return { ...p, depends_on: kept }
}

const integrateAndLand = async ({ p, review: rv }, g) => {
  if (!rv || !rv.approved) return { merged: false, note: `review rejected: ${rv ? rv.summary : 'no review'}` }
  for (const dep of p.depends_on) {
    if (!(await landed[dep])) return { merged: false, note: `dependency ${dep} did not land` }
  }
  let integ = await integrate(p, g, `Reviewer summary: ${rv.summary}`)
  // One watch per push: the first push, then one per fix round.
  for (let fixes = 0; ; fixes++) {
    if (!integ || !integ.pushed) return { merged: false, pr: integ && integ.pr_url, note: integ ? integ.failure_log || integ.summary : 'integrator died' }
    const l = await land(p, g, integ.pr_url, fixes)
    if (l && l.status === 'merged') return { merged: true, pr: integ.pr_url, note: l.summary }
    if (!l || l.status === 'gave_up' || fixes >= MAX_FIX) return { merged: false, pr: integ.pr_url, note: l ? l.failure_log || l.summary : 'lander died' }
    log(`goal ${g.id}: PR not green, fix round ${fixes + 1} of ${MAX_FIX}`)
    integ = await integrate(p, g, `FIX ROUND ${fixes + 1} of ${MAX_FIX}: the PR ${integ.pr_url} failed. Fix, re-verify, push to the same branch.\n${l.failure_log || l.summary}`)
  }
}

// Every goal settles exactly once, whatever happens to it, so a dependent can
// never wait on a goal that died, threw, or was rejected.
const runGoal = async (g) => {
  let result = { merged: false, note: 'dropped' }
  try {
    const planned = await plan(g)
    if (!planned) return (result = { merged: false, note: 'planner died' })
    const p = earlierDeps(planned, g)
    result = await integrateAndLand(await review(await work(p, g), g), g)
    return result
  } catch (e) {
    return (result = { merged: false, note: `failed: ${e && e.message ? e.message : e}` })
  } finally {
    settle[g.id](!!result.merged)
  }
}

let results
phase('Plan')
if (PARALLEL) {
  results = await parallel(args.goals.map(g => () => runGoal(g)))
} else {
  // One goal at a time, all in the local checkout, so the build cache is reused.
  results = []
  for (const g of args.goals) results.push(await runGoal(g))
}

const summary = args.goals.map((g, i) => ({ goal: g.id, ...(results[i] || { merged: false, note: 'dropped' }) }))
summary.filter(s => !s.merged).forEach(s => log(`NOT merged: goal ${s.goal}: ${s.note}`))
return summary
