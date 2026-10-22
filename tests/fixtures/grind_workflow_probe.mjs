// Controlled workflow host: no model requests, shell tools, or GitHub access.
import { readFile } from 'node:fs/promises';

const source = await readFile(process.argv[2], 'utf8');
const config = JSON.parse(await readFile(process.argv[3], 'utf8'));
const metadataExport = 'export const meta =';
if (source.split(metadataExport).length !== 2) {
  throw new Error('expected exactly one workflow metadata export');
}
// Workflow hosts execute the body as an async function, allowing its top-level
// return. Remove only the metadata export keyword; execute the production body.
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const execute = new AsyncFunction(
  'args', 'agent', 'log', 'phase', 'parallel',
  source.replace(metadataExport, 'const meta ='),
);
const prompts = [];
const agent = async (prompt, options) => {
  prompts.push({ prompt, role: options.agentType, label: options.label });
  if (options.label === 'classify') {
    return {
      confident: true,
      children: config.args.goals.map(goal => ({ id: goal.id, kind: 'bug' })),
      groups: [],
    };
  }
  throw new Error('unexpected agent in classification probe');
};
try {
  const result = await execute(
    config.args, agent, () => {}, () => {},
    tasks => Promise.all(tasks.map(task => task())),
  );
  console.log(JSON.stringify({ result, prompts }));
} catch (error) {
  console.log(JSON.stringify({ error: String(error.message || error), prompts }));
}
