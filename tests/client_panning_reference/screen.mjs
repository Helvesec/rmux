// Independent terminal interpretation, pinned by tests/xterm-oracle/package-lock.json.
import {createRequire} from 'node:module';
import fs from 'node:fs';
const require = createRequire(new URL('../xterm-oracle/package.json', import.meta.url));
const pkg = require('./node_modules/@xterm/headless/package.json');
if (pkg.version !== '6.0.0') throw Error('expected xterm 6.0.0');
const {Terminal} = require('./node_modules/@xterm/headless');
const [file, cols, rows] = process.argv.slice(2);
const raw = fs.readFileSync(file);
if (raw.length > 2*1024*1024) throw Error('capture bound exceeded');
const terminal = new Terminal({cols: +cols, rows: +rows, allowProposedApi: true});
await new Promise(resolve => terminal.write(raw, resolve));
const b = terminal.buffer.active;
const hidden = terminal._core.coreService.isCursorHidden;
if (typeof hidden !== 'boolean') throw Error('pinned xterm cursor state unavailable');
process.stdout.write(JSON.stringify({rows: Array.from({length:+rows}, (_,i)=>b.getLine(b.viewportY+i).translateToString(false)), cursor: [b.cursorX, b.cursorY], cursor_visible: !hidden}));
terminal.dispose();
