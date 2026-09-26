// managed-by: clud
export const meta = {
  name: 'grind-run',
  description: 'Engine behind clud /grind (start it with /grind): plan, write, review, integrate and land a list of goals, in parallel worktrees or sequentially in the local checkout, with per-role caps.',
  whenToUse: 'Started by the /grind router skill, which gathers the goals, mode, models and CI choice first. Do not start it directly.',
  phases: [
    { title: 'Classify', detail: 'grind-planner plan-only: classify children, feature groups, threshold' },
    { title: 'Prework', detail: 'grind-prework: post the plan JSON comment on the meta issue' },
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
//   scripts?: { lint?: './lint', test?: './test' },     // omitted = planner picks verify
//   maxAgents?: 4, maxFixRounds?: 10,
//   planOnly?: true, meta?: '<meta issue number>',
//   base?: 'main',   // default base branch for goals in no plan stage; omitted = main
//                    // a plan stage's `base` wins for its children (#1409)
//   plan?: { schema: 'grind-plan/v1', run_id, meta, original, repo, main, mode,
//            preflight, structure, stages, deferred_groups, feature_merge,
//            problem_reporting, models, ci, scripts, rules, waiting_on_pr },
//            // deferred_groups: [{ group, sub_meta, children }] feature groups
//            // skipped this run; waiting_on_pr: an open feature PR number that,
//            // with rules.no_overlap 'bugs_only', limits the run to bug stages.
//            // At most one feature stage runs per run (#1412).
//            // the router's assembled plan; when present, grind-prework posts it
//            // on the meta issue before any worker runs (#1408)
//   feature?: { branch, worktree, pr },   // set by the router after feature setup (#1410):
//                    // the feature branch, its absolute worktree
//                    // (<repo>/.clud/grind/worktrees/feature) and its draft PR
//   feature_merge?: 'auto' | 'decide_later' | 'comment_only',
//                    // what happens to the feature PR once its goals settle;
//                    // args.plan.feature_merge is also read; default decide_later
//   // problem reporting (#1411) needs no new args
// }
// problems (#1411): roles return them; the router files them per plan.problem_reporting
if (!args || !args.repo || !Array.isArray(args.goals) || !args.goals.length) {
  throw new Error('grind needs args {repo, mode, goals:[{id,title,brief}]}; start it with /grind')
}
if (!args.planOnly && args.mode !== 'parallel' && args.mode !== 'sequential') {
  throw new Error(`grind mode must be "parallel" or "sequential" (got ${JSON.stringify(args.mode)}); cron mode is driven by /grind-cron`)
}
const REPO = args.repo
const MAIN = args.main || 'main'
const PARALLEL = args.mode === 'parallel'
const CI = !!args.ci
const MAX_AGENTS = Math.max(1, Math.floor(args.maxAgents ?? 4))
const MAX_FIX = Math.max(0, Math.floor(args.maxFixRounds ?? 10))
const MODELS = args.models || {}
// The repo's ./lint and ./test, chosen once by the /grind router (#1336).
const SCRIPTS = (args.scripts && (args.scripts.lint || args.scripts.test)) ? args.scripts : null
const scriptLines = () => [SCRIPTS.lint, SCRIPTS.test].filter(Boolean)
const verifyBlock = (p) => SCRIPTS
  ? `Verify commands, in this order, before every push (fix rounds included):\n` +
    `1. planner's focused test:\n${p.verify}\n` +
    scriptLines().map((c, i) => `${i + 2}. ${c}`).join('\n') +
    `\nThe repo scripts (${scriptLines().join(', ')}) were chosen for this run; run them lint first, then test, and rerun after every fix until green.`
  : `Verify commands:\n${p.verify}`

// Model per role; the lander shares the integrator's choice, prework the planner's.
const opts = (role, label, phase, schema) => {
  const o = { label, phase, schema, agentType: `grind-${role}` }
  const m = MODELS[role === 'lander' ? 'integrator' : role === 'prework' ? 'planner' : role]
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

// Problems found outside a task (#1411): optional on every role's schema,
// recorded once per run however many rounds or roles report them.
const PROBLEMS = { type: 'array', description: 'problems found outside this task (bugs, flaky tests, doc gaps); the router files them, never you', items: { type: 'object', required: ['kind', 'summary'], properties: {
  kind: { type: 'string' }, summary: { type: 'string' }, evidence: { type: 'string' }, related_issue: { type: 'string' } } } }
const problemKey = (p) => [p.kind, p.summary, p.related_issue || ''].map(s => String(s || '').trim().toLowerCase()).join('|')
const PROBLEMS_SEEN = new Map()
const collect = (res, role, goalId) => {
  for (const p of (res && Array.isArray(res.problems) ? res.problems : [])) {
    if (!p || !String(p.summary || '').trim()) continue
    const k = problemKey(p)
    if (PROBLEMS_SEEN.has(k)) continue
    PROBLEMS_SEEN.set(k, { kind: p.kind, summary: p.summary, evidence: p.evidence, related_issue: p.related_issue, role, goal: goalId })
  }
  return res
}
const allProblems = () => [...PROBLEMS_SEEN.values()]
const problemsOf = (id) => allProblems().filter(p => String(p.goal) === String(id))
const logProblems = () => allProblems().forEach(p => log(`problem: [${p.kind}] ${p.summary} (${p.role} ${p.goal})`))

const PLAN = {
  type: 'object', required: ['checkout', 'branch', 'depends_on', 'tasks', 'verify'],
  properties: {
    checkout: { type: 'string', description: 'absolute path the goal is worked in (its worktree, or the repo in sequential mode)' },
    branch: { type: 'string' },
    depends_on: { type: 'array', items: { type: 'string' }, description: 'ids of other goals this one must land after; [] means isolated' },
    verify: { type: 'string', description: 'exact lint/build/test commands for the integrator, newline separated' },
    tasks: { type: 'array', items: { type: 'object', required: ['id', 'files', 'instructions'], properties: {
      id: { type: 'string' }, files: { type: 'array', items: { type: 'string' }, description: 'files this task writes; a check that writes nothing belongs in verify' }, instructions: { type: 'string' } } } },
    problems: PROBLEMS,
  },
}
const WORK = { type: 'object', required: ['files_touched', 'summary'], properties: {
  files_touched: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' }, blocked: { type: 'string' }, problems: PROBLEMS } }
const REVIEW = { type: 'object', required: ['approved', 'summary'], properties: {
  approved: { type: 'boolean' }, summary: { type: 'string' }, fixes_applied: { type: 'array', items: { type: 'string' } }, problems: PROBLEMS } }
const INTEG = { type: 'object', required: ['pushed', 'summary'], properties: {
  pushed: { type: 'boolean' }, pr_url: { type: 'string' }, summary: { type: 'string' }, failure_log: { type: 'string' }, problems: PROBLEMS } }
const LAND = { type: 'object', required: ['status', 'summary'], properties: {
  status: { type: 'string', enum: ['merged', 'needs_fix', 'gave_up'] }, summary: { type: 'string' }, failure_log: { type: 'string' }, problems: PROBLEMS } }

const CLASSIFY = {
  type: 'object', required: ['children', 'groups', 'order', 'confident'],
  properties: {
    children: { type: 'array', items: { type: 'object', required: ['id', 'track'], properties: {
      id: { type: 'string' }, track: { type: 'string', enum: ['bug', 'feature'] },
      group: { type: 'string', description: 'feature group name; omitted for bugs or an unplaceable feature' },
      depends_on_bugs: { type: 'array', items: { type: 'string' } } } } },
    groups: { type: 'array', items: { type: 'object', required: ['name', 'independent', 'children'], properties: {
      name: { type: 'string' }, independent: { type: 'boolean' }, children: { type: 'array', items: { type: 'string' } } } } },
    order: { type: 'array', items: { type: 'string' }, description: 'dependency order of child ids' },
    confident: { type: 'boolean' },
  },
}

// Plan-only helpers (#1406): one classification line per child, and the
// threshold that decides whether a meta issue is worth regrouping.
const slug = (s) => String(s).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40).replace(/-+$/g, '')
const featureBranch = (meta, group) => `grind/meta-${meta}-${slug(group)}`
const classificationLines = (c, meta) => (c.children || []).map(ch => {
  const id = String(ch.id).replace(/^#/, '')
  return ch.track === 'bug'
    ? `#${id} bug → ${MAIN}`
    : `#${id} feature → ${featureBranch(meta, ch.group || 'feature')}`
})
const thresholdVerdict = (c) => {
  const children = c.children || []
  const groups = c.groups || []
  const names = new Set(groups.map(g => g.name))
  if (c.confident !== true) return { path: 'simple', reason: 'classification not confident' }
  if (children.some(ch => ch.track === 'feature' && !(ch.group && names.has(ch.group)))) return { path: 'simple', reason: 'a child could not be placed' }
  if (children.length < 8) return { path: 'simple', reason: 'fewer than 8 children' }
  if (groups.filter(g => g.independent === true && (g.children || []).length >= 3).length < 2) return { path: 'simple', reason: 'fewer than 2 independent feature groups of 3+' }
  return { path: 'regroup' }
}

// Prework helpers (#1408): the plan comment is public, so local-only state
// (preflight, absolute paths) never leaves the machine.
const LOCAL_ONLY = ['repo_path', 'checkout', 'stash', 'wip', 'start_branch']
const stripLocal = (v) => Array.isArray(v)
  ? v.map(stripLocal)
  : (v && typeof v === 'object')
    ? Object.fromEntries(Object.entries(v).filter(([k]) => !LOCAL_ONLY.includes(k)).map(([k, x]) => [k, stripLocal(x)]))
    : v
const publicPlan = (p) => {
  const pub = stripLocal(JSON.parse(JSON.stringify(p)))
  if ('preflight' in pub) pub.preflight = 'handled'
  return pub
}
const PLAN_LIMIT = 65536
const planBodies = (p, limit = 60000) => {
  const cap = Math.min(limit, PLAN_LIMIT - 1)
  const pub = publicPlan(p)
  const id = pub.run_id
  const fence = (o) => '\n```json\n' + JSON.stringify(o, null, 1) + '\n```'
  const single = `<!-- grind:v1 plan run=${id} -->` + fence(pub)
  if (single.length < cap) return [single]
  const stages = Array.isArray(pub.stages) ? pub.stages : []
  // Part 1: every field, with each stage's children emptied.
  const head = { ...pub, stages: stages.map(s => (s && Array.isArray(s.children)) ? { ...s, children: [] } : s) }
  const items = []
  stages.forEach((s, i) => (s && Array.isArray(s.children) ? s.children : []).forEach(c => items.push({ i, c })))
  // Room for a worst-case marker (part=9999/9999) and fence.
  const room = cap - `<!-- grind:v1 plan run=${id} part=9999/9999 -->`.length - 16
  const chunks = []
  let cur = []
  const body = (list) => {
    const byStage = []
    list.forEach(({ i, c }) => {
      let e = byStage.find(b => b.stage === i)
      if (!e) byStage.push(e = { stage: i, name: stages[i] && stages[i].name, children: [] })
      e.children.push(c)
    })
    return JSON.stringify({ stages: byStage }, null, 1)
  }
  for (const it of items) {
    if (cur.length && body([...cur, it]).length > room) { chunks.push(cur); cur = [] }
    cur.push(it)
  }
  if (cur.length) chunks.push(cur)
  const n = chunks.length + 1
  const mark = (k) => `<!-- grind:v1 plan run=${id} part=${k}/${n} -->`
  return [
    mark(1) + fence(head) + `\nContinued in parts 2..${n} (posted below).`,
    ...chunks.map((ch, k) => mark(k + 2) + '\n```json\n' + body(ch) + '\n```'),
  ]
}
const PREWORK = { type: 'object', required: ['posted'], properties: {
  posted: { type: 'boolean' }, plan_url: { type: 'string' }, part_urls: { type: 'array', items: { type: 'string' } }, error: { type: 'string' }, problems: PROBLEMS } }

// Plan-only pass: one read-only grind-planner, no worker/reviewer/integrator/lander.
if (args.planOnly) {
  phase('Classify')
  const c = await agent(
    `Follow your built-in /grind-plan procedure in PLAN-ONLY mode (read-only: no Write/Edit, no worktree, no push, no branch).\n\nRepo: ${REPO} (default branch ${MAIN}). Meta issue: #${args.meta}.\nChildren of the meta issue:\n` +
    args.goals.map(g => `${g.id}: ${g.title}\n${g.brief}`).join('\n\n') +
    `\n\nClassify every child as bug or feature, name the feature groups and whether they are independent, which feature children depend on which bugs, and the dependency order. Return it through StructuredOutput.`,
    opts('planner', 'classify', 'Classify', CLASSIFY))
  if (!c) return { planOnly: true, path: 'simple', reason: 'planner died', lines: [] }
  const lines = classificationLines(c, args.meta)
  lines.forEach(l => log(l))
  const { path, reason } = thresholdVerdict(c)
  if (path === 'simple') log(`keeping #${args.meta} as is: ${reason}`)
  return { planOnly: true, path, reason, lines, classification: c }
}

// Prework (#1408): record the plan on the meta issue before any worker runs.
let PLAN_URL = null
if (args.plan) {
  phase('Prework')
  const bodies = planBodies(args.plan)
  const pw = collect(await agent(
    `Follow your built-in /grind-prework procedure.\n\nRepo: ${REPO} (default branch ${MAIN}). Meta issue: #${args.plan.meta}.\n` +
    `Post these ${bodies.length} comment bod${bodies.length === 1 ? 'y' : 'ies'} verbatim, in order:\n\n` +
    bodies.map((b, i) => `--- body ${i + 1} of ${bodies.length} ---\n${b}`).join('\n\n'),
    opts('prework', 'prework', 'Prework', PREWORK)), 'prework', 'prework')
  if (!pw || !pw.posted || !pw.plan_url) {
    const note = !pw ? 'prework agent died' : (pw.error || (pw.posted ? 'no plan_url returned' : 'posted=false'))
    log(`plan comment not posted: ${note}; stopping before any worker`)
    logProblems()
    return { stopped: 'prework', note, problems: allProblems() }
  }
  PLAN_URL = pw.plan_url
  log(`plan: ${PLAN_URL}`)
}

// Stages (#1409): the bugs stage lands on main first, then each feature stage
// lands on its own feature branch. A goal's base comes from its stage.
const ALL_STAGES = (args.plan && Array.isArray(args.plan.stages)) ? args.plan.stages.filter(s => s && typeof s === 'object') : []
const isBugStage = (s) => s.stage === 'bugs'
// One feature per run, no overlap (#1412): deferred groups and, under
// rules.no_overlap 'bugs_only', an open feature PR drop feature stages here.
const DEFERRED = new Set((args.plan && Array.isArray(args.plan.deferred_groups) ? args.plan.deferred_groups : [])
  .map(d => d && d.group).filter(Boolean).map(String))
const WAITING_PR = args.plan && args.plan.waiting_on_pr
const BUGS_ONLY = !!(WAITING_PR && args.plan.rules && args.plan.rules.no_overlap === 'bugs_only')
const stageName = (s) => String(s.group || s.name || s.branch || 'feature')
if (BUGS_ONLY) log(`no overlap: feature PR #${WAITING_PR} open under meta ${args.plan.meta || args.meta || '(unknown)'}; bugs only`)
const removedStages = []
let keptFeature = false
const STAGES = ALL_STAGES.filter(s => {
  if (isBugStage(s)) return true
  let note = null
  if (BUGS_ONLY) note = `no overlap: waiting on feature PR #${WAITING_PR}`
  else if (s.group && DEFERRED.has(String(s.group))) note = `deferred group ${s.group}`
  else if (keptFeature) note = `deferred group ${stageName(s)}`
  if (note) {
    log(`deferred: ${stageName(s)}`)
    removedStages.push({ s, note })
    return false
  }
  keptFeature = true
  return true
})
const keptIds = new Set()
STAGES.forEach(s => (Array.isArray(s.children) ? s.children : []).forEach(c => keptIds.add(String(c))))
const DEFERRED_GOALS = []
const deferredNote = {}
removedStages.forEach(({ s, note }) => (Array.isArray(s.children) ? s.children : []).forEach(c => {
  const id = String(c)
  if (!keptIds.has(id) && !(id in deferredNote)) deferredNote[id] = note
}))
args.goals = args.goals.filter(g => {
  const note = deferredNote[String(g.id)]
  if (note === undefined) return true
  DEFERRED_GOALS.push({ goal: g.id, id: g.id, merged: false, status: 'deferred', note, problems: [] })
  return false
})
const stageBase = {}
const stageOf = {}
STAGES.forEach(s => (Array.isArray(s.children) ? s.children : []).forEach(c => {
  const id = String(c)
  if (s.base && !(id in stageBase)) stageBase[id] = s.base
  if (!(id in stageOf)) stageOf[id] = s
}))
// Feature-branch mode (#1410): feature-stage goals land on args.feature.branch.
// No feature stage survived the #1412 filter: nothing lands on the feature branch.
const NO_FEATURE_LEFT = removedStages.length > 0 && !keptFeature
const FEATURE = (args.feature && args.feature.branch && !NO_FEATURE_LEFT) ? args.feature : null
// The router stores 'auto' | 'later' | 'comment'; the long spellings are accepted too.
const FEATURE_MERGE_ALIASES = { auto: 'auto', later: 'decide_later', decide_later: 'decide_later', comment: 'comment_only', comment_only: 'comment_only' }
const FEATURE_MERGE = [args.feature_merge, args.plan && args.plan.feature_merge]
  .map(m => FEATURE_MERGE_ALIASES[m]).find(Boolean) || 'decide_later'
const isFeatureGoal = (g) => {
  if (!FEATURE) return false
  const s = stageOf[String(g.id)]
  return s ? !isBugStage(s) : !STAGES.length
}
const baseOf = (g) => g.base || stageBase[String(g.id)] || (isFeatureGoal(g) ? FEATURE.branch : null) || args.base || MAIN
const featureCheckout = (g) => (!PARALLEL && isFeatureGoal(g))
  ? `\nThis is a feature-stage goal: work in the feature worktree ${FEATURE.worktree} (never the user's checkout ${REPO}); that is the goal's checkout.`
  : ''
const featurePlanNote = (g) => !isFeatureGoal(g) ? '' : PARALLEL
  ? `\n\nFeature-stage goal: create the goal worktree and branch from origin/${FEATURE.branch}, not origin/${MAIN}.`
  : `\n\nFeature-stage goal, sequential mode: checkout must be the feature worktree ${FEATURE.worktree}, never ${REPO}; branch from origin/${FEATURE.branch}.`
// #1393: the feature PR number, the run id, and the issue that carries the
// feature's Closes line and marker (a meta of metas' sub-meta, else the meta).
const featurePrNum = () => String(FEATURE.pr || '').replace(/\/+$/, '').replace(/^.*\//, '').replace(/^#/, '')
const FEATURE_RUN_ID = (args.plan && args.plan.run_id) || (FEATURE ? String(FEATURE.branch).replace(/^grind\/meta-\d+-/, '') : '')
const featureMetaOf = (g) => (stageOf[String(g.id)] && stageOf[String(g.id)].sub_meta) || (args.plan && args.plan.meta) || args.meta
const ON_FEATURE = 'grind:on-feature'
const featureIntegrateNote = (g) => !isFeatureGoal(g) ? '' :
  `\n\nFeature-stage goal (feature branch ${FEATURE.branch}, feature PR ${FEATURE.pr || '(none)'}, feature worktree ${FEATURE.worktree}):\n` +
  `1. Before rebasing the goal onto origin/${FEATURE.branch}: git fetch; if origin/${MAIN} has commits not in origin/${FEATURE.branch}, ` +
  `merge ${MAIN} into the feature branch in the feature worktree (first git merge --ff-only origin/${FEATURE.branch}, then git merge --no-ff origin/${MAIN}), then plain push of ${FEATURE.branch} (no force). ` +
  `Never rebase the feature branch.\n` +
  `2. Rebase the goal branch onto origin/${FEATURE.branch}. The goal PR's base is ${FEATURE.branch}, not ${MAIN}; its body uses \`Refs #${g.id}\`, not Closes.\n` +
  `3. Do not edit the feature PR: the lander adds \`Closes #${g.id}\` to it only once this goal has landed on the feature branch, so a goal that never lands is never closed by the feature merge.`
const featureLandNote = (g) => {
  if (!isFeatureGoal(g)) return ''
  const fpr = featurePrNum() || '<feature pr>'
  const meta = featureMetaOf(g)
  const original = args.plan && args.plan.original
  const marker = (goalPr) => `<!-- grind:v1 feature-pr=#${fpr} branch=${FEATURE.branch}${goalPr ? ` goal-pr=#${goalPr}` : ''} run=${FEATURE_RUN_ID || '<run-id>'} -->`
  const others = [meta, original].filter(Boolean).map(n => `#${n}`)
  return `\n\nFeature-stage goal: merge this PR into the feature branch ${FEATURE.branch} with \`gh pr merge <n> --admin --merge\` (never --delete-branch), never into ${MAIN}.\n` +
    `Once it has merged, record the landing so issue #${g.id} is never lost (#1393):\n` +
    `1. gh label create ${ON_FEATURE} --force\n` +
    `2. gh issue edit ${g.id} --add-label ${ON_FEATURE}` + (others.length ? `, and the same for ${others.join(' and ')}` : '') + `.\n` +
    `3. gh issue comment ${g.id} --body 'Landed on feature branch ${FEATURE.branch} via #<n>; closes when feature PR #${fpr} merges into ${MAIN}. ${marker('<n>')}'` +
    (others.length ? `. For ${others.join(' and ')}: read its comments (gh issue view <m> --json comments) and, only if none has a marker with feature-pr=#${fpr}, post one: ${marker(null)}` : '') + `.\n` +
    `4. gh pr view ${fpr} --json body, then gh pr edit ${fpr} --body '<body>' with a \`Closes #${g.id}\` line added and the goals table row for #${g.id} updated; keep every other line, including the other Closes lines. ` +
    `View it again and repeat the edit if \`Closes #${g.id}\` is missing (another lander may have edited the body at the same time).\n` +
    `Never gh issue close; the feature PR's Closes lines close the issues when it merges into ${MAIN}.`
}
const STUCK_BUG_BLOCKS = !!(args.plan && args.plan.rules && args.plan.rules.stuck_bug === 'block_dependents_only')
const bugDepsOf = (g) => {
  const s = stageOf[String(g.id)]
  if (!s || isBugStage(s) || !s.depends_on_bugs || typeof s.depends_on_bugs !== 'object') return []
  const entry = Object.entries(s.depends_on_bugs).find(([k]) => String(k) === String(g.id))
  return entry && Array.isArray(entry[1]) ? entry[1].map(String) : []
}

const ctx = (g) => `Repo: ${REPO} (default branch ${MAIN}). Mode: ${args.mode}. Local CI (act): ${CI ? `on, lane ${args.lane || '(pick from ci.yml)'}` : 'off'}.\nGoal ${g.id}: ${g.title}\n${g.brief}` +
  (PLAN_URL ? `\nPlan comment: ${PLAN_URL} (read it before you start; it is the recorded plan and never changes).` : '') +
  featureCheckout(g)
const others = (g) => args.goals.filter(o => o.id !== g.id).map(o => `${o.id}: ${o.title}`).join('\n') || '(none)'

const plan = (g) => light(() => agent(
  `Follow your built-in /grind-plan procedure.\n\n${ctx(g)}\nBase branch: ${baseOf(g)} (branch from origin/${baseOf(g)}).\n\nOther goals in this run (for depends_on):\n${others(g)}` +
  (SCRIPTS ? `\n\nRun scripts chosen: ${scriptLines().join(', ')}. The integrator runs them before every push, so verify must hold only the goal's focused test; do not add lint or test commands.` : '') +
  featurePlanNote(g),
  opts('planner', `plan:${g.id}`, 'Plan', PLAN)).then(r => collect(r, 'planner', g.id)))

const work = (p, g) => (PARALLEL
  ? parallel(p.tasks.map(t => () => light(() => runTask(p, g, t))))
  : p.tasks.reduce((acc, t) => acc.then(rs => runTask(p, g, t).then(r => [...rs, r])), Promise.resolve([])))
  .then(rs => ({ p, results: rs.filter(Boolean) }))
const runTask = (p, g, t) => agent(
  `Follow your built-in /grind-work procedure.\n\n${ctx(g)}\n\nCheckout: ${p.checkout}\nTask ${t.id}. Files you own: ${t.files.join(', ')}\n\n${t.instructions}`,
  opts('worker', `work:${g.id}/${t.id}`, 'Work', WORK)).then(r => collect(r, 'worker', g.id))

const review = ({ p, results }, g) => light(() => agent(
  `Follow your built-in /grind-review procedure.\n\n${ctx(g)}\n\nCheckout: ${p.checkout}\nWorker reports:\n${JSON.stringify(results, null, 1)}`,
  opts('reviewer', `review:${g.id}`, 'Review', REVIEW)).then(r => collect(r, 'reviewer', g.id))).then(r => ({ p, review: r }))

const integrate = (p, g, note) => exclusive(() => agent(
  `Follow your built-in /grind-integrate procedure.\n\n${ctx(g)}\n\nCheckout: ${p.checkout}\nBranch: ${p.branch}\n` +
  `Base: origin/${baseOf(g)}${p.depends_on.length ? ` (it already contains ${p.depends_on.join(', ')}, which landed first)` : ''}\n` +
  `${verifyBlock(p)}\n\n${note}${featureIntegrateNote(g)}`,
  opts('integrator', `integrate:${g.id}`, 'Integrate', INTEG)).then(r => collect(r, 'integrator', g.id)))

const land = (p, g, pr, round) => agent(
  `Follow your built-in /grind-land procedure.\n\n${ctx(g)}\n\nPR: ${pr}\nBranch: ${p.branch}\nFix rounds used: ${round} of ${MAX_FIX}.${featureLandNote(g)}`,
  opts('lander', `land:${g.id}#${round}`, 'Land', LAND)).then(r => collect(r, 'lander', g.id))

// Dependents wait until what they depend on has merged, then rebase onto the
// new origin/<base> of their stage (main for bugs, the feature branch for a
// feature stage); isolated goals go straight to origin/<base>.
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

// Workers and reviewers cannot run commands (#1397), so a task that writes no
// file is a check: hand it to the integrator's verify instead of a worker.
const routeCheckTasks = (p) => {
  const tasks = p.tasks || []
  const writes = (t) => (t.files || []).length > 0
  const checks = tasks.filter(t => !writes(t))
  if (!checks.length) return { ...p, tasks }
  log(`moving file-less task(s) ${checks.map(t => t.id).join(', ')} into verify`)
  return {
    ...p,
    tasks: tasks.filter(writes),
    verify: [p.verify, ...checks.map(t => `# from planned task ${t.id} (may be prose; run the equivalent checks):\n${t.instructions}`)]
      .filter(Boolean).join('\n'),
  }
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
    integ = await integrate(p, g, `FIX ROUND ${fixes + 1} of ${MAX_FIX}: the PR ${integ.pr_url} failed. Fix, re-verify${SCRIPTS ? ` (focused test, then ${scriptLines().join(', then ')})` : ''}, push to the same branch.\n${l.failure_log || l.summary}`)
  }
}

// Every goal settles exactly once, whatever happens to it, so a dependent can
// never wait on a goal that died, threw, or was rejected.
const runGoal = async (g) => {
  let result = { merged: false, note: 'dropped' }
  try {
    // Stuck-bug rule: a failed bug blocks only the feature children that list it.
    if (STUCK_BUG_BLOCKS) {
      for (const bug of bugDepsOf(g)) {
        if (!(bug in landed)) continue
        if (!(await landed[bug])) return (result = { merged: false, note: `blocked: bug #${bug} did not land` })
      }
    }
    const planned = await plan(g)
    if (!planned) return (result = { merged: false, note: 'planner died' })
    const p = routeCheckTasks(earlierDeps(planned, g))
    if (!p.tasks.length) return (result = { merged: false, note: 'plan had no file-writing tasks; nothing for workers to do' })
    result = await integrateAndLand(await review(await work(p, g), g), g)
    return result
  } catch (e) {
    return (result = { merged: false, note: `failed: ${e && e.message ? e.message : e}` })
  } finally {
    settle[g.id](!!result.merged)
  }
}

const runBatch = async (goals) => {
  if (PARALLEL) return parallel(goals.map(g => () => runGoal(g)))
  // One goal at a time, all in the local checkout, so the build cache is reused.
  const rs = []
  for (const g of goals) rs.push(await runGoal(g))
  return rs
}

let results
phase('Plan')
const featureStages = STAGES.filter(s => !isBugStage(s))
if (STAGES.some(isBugStage) && featureStages.length) {
  // Bug stage first (plus any goal in no stage), then each feature stage in order.
  const featureIds = new Set()
  const batches = featureStages.map(s => {
    const ids = new Set((Array.isArray(s.children) ? s.children : []).map(String))
    const goals = args.goals.filter(g => ids.has(String(g.id)) && !featureIds.has(String(g.id)))
    goals.forEach(g => featureIds.add(String(g.id)))
    return { label: `stage feature ${s.group || s.name || s.branch || 'feature'}`, goals }
  })
  batches.unshift({ label: 'stage bugs', goals: args.goals.filter(g => !featureIds.has(String(g.id))) })
  const byId = {}
  for (const b of batches) {
    log(`${b.label}: ${b.goals.length} goal(s)`)
    const rs = await runBatch(b.goals)
    b.goals.forEach((g, i) => { byId[String(g.id)] = rs[i] })
  }
  results = args.goals.map(g => byId[String(g.id)])
} else {
  results = await runBatch(args.goals)
}

const summary = args.goals.map((g, i) => ({ goal: g.id, ...(results[i] || { merged: false, note: 'dropped' }), problems: problemsOf(g.id) }))
summary.filter(s => !s.merged).forEach(s => log(`NOT merged: goal ${s.goal}: ${s.note}`))
DEFERRED_GOALS.forEach(d => {
  log(`deferred: goal ${d.goal}: ${d.note}`)
  summary.push(d)
})
if (PLAN_URL) log(`plan: ${PLAN_URL}`)
if (!FEATURE) {
  logProblems()
  return summary
}

// Feature landing (#1410): once every feature-stage goal has settled.
const feature = { branch: FEATURE.branch, pr: FEATURE.pr || null, policy: FEATURE_MERGE, merged: false, note: '' }
if (!FEATURE.pr) {
  feature.note = 'no feature PR'
} else if (FEATURE_MERGE === 'auto') {
  phase('Land')
  const l = collect(await agent(
    `Follow your built-in /grind-land procedure for the FEATURE PR.\n\nRepo: ${REPO} (default branch ${MAIN}).\nFeature PR: ${FEATURE.pr}\nFeature branch: ${FEATURE.branch}\n\n` +
    `1. gh pr ready ${FEATURE.pr}\n2. Wait for CI with github/pr_merge_watch.\n` +
    `3. gh pr merge ${FEATURE.pr} --merge (no --admin, no --squash, no --delete-branch).\n` +
    `If a review is required and missing, return status 'gave_up' with summary 'waiting for review'; do not retry.`,
    opts('lander', 'land:feature', 'Land', LAND)), 'lander', 'feature')
  feature.merged = !!(l && l.status === 'merged')
  feature.note = l ? (l.failure_log || l.summary) : 'lander died'
} else if (FEATURE_MERGE === 'comment_only') {
  feature.note = 'the router posts the result comment; feature PR stays draft'
} else {
  feature.note = 'feature PR left open for the user; the router marks it ready once every feature goal landed'
}
log(`feature ${feature.branch}: PR ${feature.pr || '(none)'}, policy ${feature.policy}, ${feature.merged ? 'merged' : 'not merged'}: ${feature.note}`)
feature.problems = problemsOf('feature')
logProblems()
return { goals: summary, feature, problems: allProblems() }
