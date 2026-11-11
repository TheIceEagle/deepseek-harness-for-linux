// A normal Cordis tool plugin, loaded without adapting its exports or services.
import { defineTool } from '@deepseek-ai/dsh-tools';
export const name = 'native-compat-fixture';
export const inject = ['tools'];
export function apply(ctx) {
  ctx.tools.register(defineTool({
    name: 'compat_echo',
    description: 'Return supplied text using the real Cordis tool pipeline.',
    parameters: { text: { type: 'string', required: true } },
    output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
    execute(args) { return args.text; },
    // Exercise the bridge's final output cap after upstream spill policies.
    finalizeContent(exec) {
      if (exec.arguments.text.length >= 70000) return [{ type: 'text', text: exec.arguments.text }];
    },
  }));
  ctx.tools.register(defineTool({
    name: 'compat_wait',
    description: 'Wait until the caller cancels this fixture operation.',
    parameters: {},
    output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
    async execute(_args, exec) {
      await new Promise((resolve) => {
        if (exec.signal.aborted) resolve();
        else exec.signal.addEventListener('abort', resolve, { once: true });
      });
      return 'wait ended';
    },
  }));
  ctx.tools.register(defineTool({
    name: 'compat_environment',
    description: 'Check approved environment forwarding without returning credential values.',
    parameters: { approved_github: { type: 'boolean' } },
    output: { schema: { type: 'boolean' }, render: (_args, value) => [{ type: 'text', text: String(value) }] },
    execute(args) {
      const credentials = Object.keys(process.env).filter(name => /KEY|PASSWORD|SECRET|TOKEN/i.test(name));
      if (args.approved_github) return Boolean(process.env.GITHUB_TOKEN)
        && credentials.length === 1 && credentials[0] === 'GITHUB_TOKEN'
        && process.env.DEEPSEEK_API_KEY === undefined
        && process.env.UNAPPROVED_SECRET === undefined
        && process.env.DSH_UNTRUSTED_TOKEN === undefined;
      return credentials.length > 0;
    },
  }));
  console.log('Ordinary plugin logging must not enter the MCP stream.');
}
