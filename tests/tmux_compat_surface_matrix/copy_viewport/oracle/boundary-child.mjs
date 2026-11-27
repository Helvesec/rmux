// Controlled protocol faults, using independently recorded xterm rows. This
// child is not a terminal implementation and does not describe normal xterm.
import fs from 'node:fs';
import readline from 'node:readline';
import assert from 'node:assert/strict';
const [mode, dir, rowsFile, rawFile] = process.argv.slice(2);
const rows = JSON.parse(fs.readFileSync(rowsFile)).baseline;
const raw = Buffer.from(fs.readFileSync(rawFile, 'utf8').trim(), 'hex');
function after(marker, action) {
  const timer = setInterval(() => {
    if (!fs.existsSync(`${dir}/${marker}`)) return;
    clearInterval(timer);
    action();
  }, 1);
}
if (mode === 'nonreading') {
  after('drain', () => {
    assert.equal(fs.readSync(0, Buffer.alloc(8192), 0, 8192, null), 8192);
    fs.writeFileSync(`${dir}/drained`, '8192');
    setInterval(() => {}, 1000);
  });
} else {
  readline.createInterface({ input: process.stdin }).once('line', line => {
    const q = JSON.parse(line);
    assert.equal(q.sequence, 1);
    assert.equal(q.end, raw.length);
    assert.equal(q.reset, false);
    assert.deepEqual(Buffer.from(q.bytes), raw);
    fs.writeFileSync(`${dir}/request-writing`, line);
    fs.renameSync(`${dir}/request-writing`, `${dir}/request`);
    after('release', () => {
      const ack = JSON.stringify({ protocol: 1, xterm: '6.0.0', sequence: q.sequence, end: q.end, rows });
      const padded = ack + ' '.repeat(8191 - Buffer.byteLength(ack)) + '\n';
      if (mode === 'continuous') {
        for (;;) fs.writeSync(1, ' '.repeat(8192));
      } else if (mode === 'fragment') {
        fs.writeSync(1, (ack + '\n').slice(0, 17));
        after('tail', () => fs.writeSync(1, (ack + '\n').slice(17)));
      } else {
        const text = mode === 'exit7' || mode === 'residue' ? ack + '\n' : padded;
        fs.writeSync(1, text + (mode === 'duplicate' ? ack + '\n' : mode === 'extra' ? '{' : ''));
        if (mode === 'exit7') process.exit(7);
        if (mode === 'residue') after('tail', () => fs.writeSync(1, '{'));
      }
    });
  });
}
