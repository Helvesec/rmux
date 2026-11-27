// Incremental 80x24 text oracle for the attached copy-mode tests. The recovery
// oracle uses the same pinned package and buffer row API. No product parser.
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const MAX_LINE = 65536;
const MAX_BYTES = 1048576;
const MAX_REPLY = 32768;
let terminal;

async function run() {
  // Resolve only this checkout's installation, never an ancestor/global module.
  const pkg = require('./node_modules/@xterm/headless/package.json');
  const expected = require('./package-lock.json').packages['node_modules/@xterm/headless'].version;
  if (pkg.version !== expected || expected !== '6.0.0') throw Error('expected pinned xterm 6.0.0');
  const { Terminal } = require('./node_modules/@xterm/headless');
  const fresh = () => new Terminal({ cols: 80, rows: 24, scrollback: 1000, allowProposedApi: true });
  terminal = fresh();
  if (process.argv[2] === '--check') return;
  let received = 0;
  let sequence = 0;
  let requests = 0;
  let pending = Buffer.alloc(0);
  // Pausing at each async iteration also bounds Node's input queue. The caller
  // permits only one request in flight and waits for its write callback reply.
  for await (const chunk of process.stdin) {
    if (pending.length + chunk.length > MAX_LINE) throw Error('oracle input limit');
    pending = Buffer.concat([pending, chunk]);
    let newline;
    while ((newline = pending.indexOf(10)) !== -1) {
      const request = JSON.parse(pending.subarray(0, newline).toString('utf8'));
      pending = pending.subarray(newline + 1);
      if (request.sequence !== sequence + 1 || !Array.isArray(request.bytes)
          || request.bytes.length > 8192
          || !request.bytes.every(b => Number.isInteger(b) && b >= 0 && b <= 255)
          || typeof request.reset !== 'boolean') throw Error('invalid oracle request');
      sequence = request.sequence;
      if (request.reset) {
        terminal.dispose();
        terminal = fresh();
        received = 0;
        requests = 0;
      }
      received += request.bytes.length;
      if (received > MAX_BYTES || ++requests > 4096 || request.end !== received) {
        throw Error('oracle byte/work limit or offset mismatch');
      }
      if (request.bytes.length) {
        await new Promise(resolve => terminal.write(Uint8Array.from(request.bytes), resolve));
      }
      const buffer = terminal.buffer.active;
      const rows = Array.from({ length: 24 }, (_, row) =>
        buffer.getLine(buffer.viewportY + row).translateToString(false).replace(/ +$/, ''));
      const reply = JSON.stringify({ protocol: 1, xterm: pkg.version, sequence, end: received, rows }) + '\n';
      if (Buffer.byteLength(reply) > MAX_REPLY) throw Error('oracle text reply limit');
      await new Promise((resolve, reject) => process.stdout.write(reply, error => error ? reject(error) : resolve()));
    }
  }
  if (pending.length) throw Error('truncated oracle request at EOF');
}

try {
  await run();
} catch (error) {
  // A bounded protocol error remains readable even when stderr is unavailable.
  process.stdout.write(JSON.stringify({ error: `xterm viewport oracle: ${String(error.message).slice(0, 512)}; run npm --prefix tests/xterm-oracle ci --ignore-scripts` }) + '\n');
  process.exitCode = 1;
} finally {
  terminal?.dispose();
}
