import xtermHeadless from '@xterm/headless';

const { Terminal } = xtermHeadless;

let inputText = '';
process.stdin.setEncoding('utf8');
for await (const chunk of process.stdin) {
  inputText += chunk;
}
const input = JSON.parse(inputText);

function write(terminal, bytes) {
  return new Promise((resolve) => {
    terminal.write(Uint8Array.from(bytes), resolve);
  });
}

function optional(cell, method) {
  return typeof cell[method] === 'function' ? cell[method]() : null;
}

function capture(terminal, title) {
  const buffer = terminal.buffer.active;
  const lines = [];
  for (let row = 0; row < buffer.length; row += 1) {
    const line = buffer.getLine(row);
    if (!line) {
      lines.push(null);
      continue;
    }
    const cells = [];
    for (let col = 0; col < terminal.cols; col += 1) {
      const cell = line.getCell(col);
      const width = cell?.getWidth();
      const chars = cell?.getChars();
      cells.push(cell ? {
        // xterm distinguishes an untouched default cell from an explicitly
        // written default space in its allocation details. They are the same
        // terminal surface state; retain empty width-zero wide continuations.
        chars: chars === '' && width === 1 ? ' ' : chars,
        width,
        bold: optional(cell, 'isBold'),
        dim: optional(cell, 'isDim'),
        italic: optional(cell, 'isItalic'),
        underline: optional(cell, 'isUnderline'),
        blink: optional(cell, 'isBlink'),
        inverse: optional(cell, 'isInverse'),
        invisible: optional(cell, 'isInvisible'),
        strike: optional(cell, 'isStrikethrough'),
        overline: optional(cell, 'isOverline'),
        fgMode: optional(cell, 'getFgColorMode'),
        fg: optional(cell, 'getFgColor'),
        bgMode: optional(cell, 'getBgColorMode'),
        bg: optional(cell, 'getBgColor')
      } : null);
    }
    lines.push({
      wrapped: line.isWrapped,
      text: line.translateToString(false),
      cells
    });
  }
  return {
    cols: terminal.cols,
    rows: terminal.rows,
    title,
    type: buffer.type,
    cursorX: buffer.cursorX,
    cursorY: buffer.cursorY,
    viewportY: buffer.viewportY,
    baseY: buffer.baseY,
    length: buffer.length,
    modes: terminal.modes,
    lines
  };
}

async function run(bytes, tail, vector) {
  const terminal = new Terminal({
    cols: vector.cols,
    rows: vector.rows,
    scrollback: vector.scrollback,
    allowProposedApi: true
  });
  let title = '';
  terminal.onTitleChange((value) => {
    title = value;
  });
  await write(terminal, bytes);
  await write(terminal, tail);
  const state = capture(terminal, title);
  terminal.dispose();
  return state;
}

for (const vector of input.vectors) {
  const expected = await run(vector.initial, vector.tail, vector);
  const actual = await run(
    [...(vector.actualPrefix ?? []), ...vector.keyframe],
    vector.tail,
    vector
  );
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    process.stderr.write(`${vector.name}: xterm.js state mismatch\n`);
    const summarize = (state) => ({
      title: state.title,
      type: state.type,
      cursorX: state.cursorX,
      cursorY: state.cursorY,
      viewportY: state.viewportY,
      baseY: state.baseY,
      modes: state.modes,
      lines: state.lines.map((line) => line && ({
        wrapped: line.wrapped,
        text: line.text
      }))
    });
    process.stderr.write(`expected=${JSON.stringify(summarize(expected))}\n`);
    process.stderr.write(`actual=${JSON.stringify(summarize(actual))}\n`);
    process.exit(1);
  }
}

process.stdout.write(`xterm-recovery-oracle=ok vectors=${input.vectors.length}\n`);
