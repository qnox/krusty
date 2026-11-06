const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, 'release-build-progress.cjs'), 'utf8');
const headers = ['Image Name', 'PID', 'Session Name', 'Session#', 'Mem Usage', 'Status', 'User Name', 'CPU Time', 'Window Title'];
const row = (image, pid, memory = '1,048,576 K', cpu = '1:02:03') =>
  [image, pid, 'Console', '1', memory, 'Running', 'PRIVATE_USER', cpu, 'PRIVATE_TITLE, "quoted"\nsecond line'];
const csv = rows => rows.map(cells => cells.map(cell => `"${String(cell).replaceAll('"', '""')}"`).join(',')).join('\r\n') + '\r\n';

function run(outputs, platform = 'win32') {
  const logs = [], calls = [], timers = [];
  vm.runInNewContext(source, {
    require(name) {
      if (name === 'node:os') return {
        platform: () => platform, arch: () => 'arm64', cpus: () => [0, 0, 0, 0],
        totalmem: () => 16 * 1024 ** 3, freemem: () => 8 * 1024 ** 3,
      };
      assert.equal(name, 'node:child_process');
      return { execFileSync(file, args, options) {
        calls.push({ file, args: Array.from(args), options });
        const output = outputs.shift();
        if (output instanceof Error) throw output;
        return output;
      } };
    },
    console: { error: value => logs.push(String(value)) },
    setInterval: (callback, ms) => timers.push({ callback, ms }), Buffer, Date,
  });
  return { logs, calls, timers, text: () => logs.join('\n') };
}

test('native CSV query emits only selected process metrics, even with quoted private columns', () => {
  const result = run(['\uFEFF' + csv([headers, row('unrelated.exe', '99'), row('RUSTC.EXE', '42'), row('link.exe', '43', '2,048 K', '0:00:07')])]);
  assert.equal(result.calls[0].file, 'tasklist.exe');
  assert.deepEqual(result.calls[0].args, ['/V', '/FO', 'CSV']);
  assert.equal(result.calls[0].options.timeout, 15000);
  assert.match(result.text(), /42 rustc\.exe 3723 1024/);
  assert.match(result.text(), /43 link\.exe 7 2/);
  assert.doesNotMatch(result.text(), /PRIVATE_|unrelated\.exe|Session Name|Window Title/);
});

test('actual headers own column positions and repeated snapshots retain cumulative CPU time', () => {
  const order = [7, 8, 4, 0, 6, 1, 2, 3, 5];
  const reorder = cells => order.map(index => cells[index]);
  const result = run([
    csv([reorder(headers), reorder(row('rustc.exe', '42'))]),
    csv([reorder(headers), reorder(row('rustc.exe', '42', '2.048 K', '1:03:03'))]),
  ]);
  assert.equal(result.timers.length, 1);
  assert.equal(result.timers[0].ms, 60000);
  result.timers[0].callback();
  assert.equal(result.calls.length, 2);
  assert.match(result.text(), /42 rustc\.exe 3723 1024/);
  assert.match(result.text(), /42 rustc\.exe 3783 2/);
  assert.match(result.text(), /CPU is cumulative seconds/);
});

test('a valid snapshot with no selected process is explicitly empty', () => {
  const result = run([csv([headers, row('unrelated.exe', '99')])]);
  assert.match(result.text(), /no compiler or linker processes present/);
  assert.doesNotMatch(result.text(), /unavailable|PRIVATE_|unrelated/);
});

test('invalid CSV, schema, or selected numeric data fails without exposing native rows', async t => {
  const cases = [
    '',
    '"PRIVATE_UNTERMINATED',
    csv([['PRIVATE_WRONG_HEADER'], ['PRIVATE_DATA']]),
    csv([[...headers, 'PID'], [...row('rustc.exe', '42'), '42']]),
    csv([headers, row('rustc.exe', 'PRIVATE_PID')]),
    csv([headers, row('rustc.exe', '42', '1.5 K')]),
    csv([headers, row('rustc.exe', '42', '1,024 K', '0:99:00')]),
    csv([headers, row('rustc.exe', '9007199254740992')]),
    csv([headers, row('rustc.exe', '42').slice(0, 7)]),
    csv([headers, row('rustc.exe', '42'), row('rustc.exe', '43', '1 K', 'PRIVATE_CPU')]),
  ];
  for (const [index, input] of cases.entries()) await t.test(String(index), () => {
    const result = run([input]);
    assert.match(result.text(), /resource snapshot unavailable \(TASKLIST_/);
    assert.doesNotMatch(result.text(), /PRIVATE_|42 rustc\.exe|no compiler or linker processes present/);
  });
});

test('native query failures expose status without raw stderr or partial private output', () => {
  const error = Object.assign(new Error('PRIVATE_EXCEPTION'), {
    status: 5, stderr: Buffer.from('PRIVATE_STDERR'), stdout: Buffer.from(csv([headers, row('rustc.exe', '42')])),
  });
  const result = run([error]);
  assert.match(result.text(), /resource snapshot unavailable \(5\)/);
  assert.doesNotMatch(result.text(), /PRIVATE_|rustc\.exe|no compiler or linker processes present/);
});

test('Unix query and process projection remain executable', () => {
  const result = run(['10 1 100 2048 00:01 /tool/rustc\n11 1 0 1024 00:02 /private/unrelated\n'], 'darwin');
  assert.equal(result.calls[0].file, 'ps');
  assert.deepEqual(result.calls[0].args, ['-A', '-o', 'pid=,ppid=,%cpu=,rss=,etime=,comm=']);
  assert.match(result.text(), /10 1 100 2048 00:01 \/tool\/rustc/);
  assert.doesNotMatch(result.text(), /private|unrelated/);
});
