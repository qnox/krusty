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

const cpu = (user = 0, sys = 0, idle = 0, nice = 0, irq = 0) => ({
  times: { user, sys, idle, nice, irq }, model: 'PRIVATE_MODEL', speed: 'PRIVATE_SPEED',
});

function run(outputs, platform = 'win32', cpuOutputs = []) {
  const logs = [], calls = [], timers = [];
  let cpuReads = 0;
  vm.runInNewContext(source, {
    require(name) {
      if (name === 'node:os') return {
        platform: () => platform, arch: () => 'arm64', cpus: () => {
          const next = cpuReads++ === 0 ? undefined : cpuOutputs.shift();
          if (next instanceof Error) throw next;
          return next === undefined ? [cpu(), cpu(), cpu(), cpu()] : next;
        },
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
  const result = run(['10 1 100 2048 00:01 /tool/rustc\n11 1 0 1024 00:02 /private/unrelated\n'], 'linux');
  assert.equal(result.calls[0].file, 'ps');
  assert.deepEqual(result.calls[0].args, ['-A', '-o', 'pid=,ppid=,%cpu=,rss=,etime=,comm=']);
  assert.match(result.text(), /10 1 100 2048 00:01 \/tool\/rustc/);
  assert.doesNotMatch(result.text(), /private|unrelated/);
});

const memoryFields = {
  'Pages free': 10, 'Pages active': 20, 'Pages inactive': 30, 'Pages speculative': 40,
  'Pages wired down': 50, 'Pages purgeable': 60, 'Pages occupied by compressor': 70,
  Pageouts: 100, Swapouts: 200, Compressions: 300, Decompressions: 400,
};
const vmStats = (pageSize = 16384, overrides = {}) =>
  `Mach Virtual Memory Statistics: (page size of ${pageSize} bytes)\n`
  + Object.entries({ ...memoryFields, ...overrides }).map(([name, value]) => `${name}: ${value}.\n`).join('')
  + 'Unselected private detail: PRIVATE_VALUE\n';
const memoryLines = result => result.logs.filter(line => line.startsWith('release build: vm_stat '));

test('Darwin snapshots preserve native page size and whitelist numeric counts only', () => {
  const result = run(['', vmStats()], 'darwin');
  assert.equal(result.calls.length, 2);
  assert.equal(result.calls[1].file, 'vm_stat');
  assert.deepEqual(result.calls[1].args, []);
  assert.equal(result.calls[1].options.timeout, 15000);
  assert.equal(result.calls[1].options.maxBuffer, 65536);
  assert.deepEqual(memoryLines(result), [
    'release build: vm_stat page_size_bytes=16384 free_pages=10 active_pages=20 inactive_pages=30 speculative_pages=40 wired_pages=50 purgeable_pages=60 compressor_pages=70 pageouts_pages_total=100 swapouts_pages_total=200 compressions_pages_total=300 decompressions_pages_total=400 counter_deltas=baseline',
  ]);
  assert.doesNotMatch(result.text(), /PRIVATE_|Unselected|_delta=/);
});

test('Darwin activity deltas use consecutive snapshots on the existing cadence', () => {
  const result = run(['', vmStats(4096), '', vmStats(4096, {
    Pageouts: 105, Swapouts: 202, Compressions: 310, Decompressions: 407,
  })], 'darwin');
  assert.equal(result.timers.length, 1);
  assert.equal(result.timers[0].ms, 60000);
  result.timers[0].callback();
  assert.deepEqual(result.calls.map(call => call.file), ['ps', 'vm_stat', 'ps', 'vm_stat']);
  assert.deepEqual(memoryLines(result), [
    'release build: vm_stat page_size_bytes=4096 free_pages=10 active_pages=20 inactive_pages=30 speculative_pages=40 wired_pages=50 purgeable_pages=60 compressor_pages=70 pageouts_pages_total=100 swapouts_pages_total=200 compressions_pages_total=300 decompressions_pages_total=400 counter_deltas=baseline',
    'release build: vm_stat page_size_bytes=4096 free_pages=10 active_pages=20 inactive_pages=30 speculative_pages=40 wired_pages=50 purgeable_pages=60 compressor_pages=70 pageouts_pages_total=105 pageouts_pages_delta=5 swapouts_pages_total=202 swapouts_pages_delta=2 compressions_pages_total=310 compressions_pages_delta=10 decompressions_pages_total=407 decompressions_pages_delta=7 counter_deltas=available',
  ]);
});

test('a counter reset or page-size change establishes a fresh delta baseline', async t => {
  for (const [name, second] of [
    ['counter reset', vmStats(16384, { Pageouts: 1 })],
    ['page-size change', vmStats(4096)],
  ]) await t.test(name, () => {
    const result = run(['', vmStats(), '', second, '', second], 'darwin');
    result.timers[0].callback();
    assert.match(memoryLines(result)[1], /counter_deltas=reset$/);
    assert.doesNotMatch(memoryLines(result)[1], /_delta=/);
    result.timers[0].callback();
    assert.match(memoryLines(result)[2], /pageouts_pages_delta=0/);
    assert.match(memoryLines(result)[2], /counter_deltas=available$/);
  });
});

test('malformed Darwin snapshots never emit partial metrics or raw output', async t => {
  const cases = [
    '', 'PRIVATE_HEADER', vmStats(0), vmStats(1.5),
    vmStats().replace('Pageouts: 100.\n', ''),
    vmStats() + 'Pageouts: 10.\n',
    vmStats(16384, { Pageouts: -1 }),
    vmStats(16384, { Pageouts: '9007199254740992' }),
    vmStats(16384, { 'Pages free': 'PRIVATE_COUNT' }),
  ];
  for (const [index, output] of cases.entries()) await t.test(String(index), () => {
    const result = run(['', output], 'darwin');
    assert.deepEqual(memoryLines(result), ['release build: vm_stat snapshot unavailable (VMSTAT_DATA)']);
    assert.doesNotMatch(result.text(), /PRIVATE_|page_size_bytes=/);
  });
});

test('Darwin query failure resets deltas without raw exception output', () => {
  const failure = Object.assign(new Error('PRIVATE_EXCEPTION'), {
    code: 'ETIMEDOUT', stderr: Buffer.from('PRIVATE_STDERR'), stdout: Buffer.from('PRIVATE_STDOUT'),
  });
  const result = run(['', vmStats(), '', failure, '', vmStats()], 'darwin');
  result.timers[0].callback();
  result.timers[0].callback();
  assert.match(memoryLines(result)[1], /snapshot unavailable \(ETIMEDOUT\)$/);
  assert.match(memoryLines(result)[2], /counter_deltas=baseline$/);
  assert.doesNotMatch(memoryLines(result)[2], /_delta=/);
  assert.doesNotMatch(result.text(), /PRIVATE_/);
});

test('Darwin memory reporting survives a failed process snapshot, and other platforms never run it', () => {
  const result = run([Object.assign(new Error('process failure'), { status: 5 }), vmStats()], 'darwin');
  assert.match(memoryLines(result)[0], /counter_deltas=baseline$/);
  for (const [platform, output] of [['linux', ''], ['win32', csv([headers])]]) {
    const other = run([output], platform);
    assert.equal(other.calls.length, 1);
    assert.notEqual(other.calls[0].file, 'vm_stat');
  }
});
const cpuLines = result => result.logs.filter(line => line.startsWith('release build: host_cpu '));

test('host CPU output preserves separate native cumulative modes and consecutive per-core deltas', () => {
  const result = run(['', ''], 'linux', [[cpu(100, 200, 300, 40, 50), cpu(10, 20, 30, 4, 5)],
    [cpu(107, 211, 313, 57, 69), cpu(11, 22, 33, 8, 10)]]);
  result.timers[0].callback();
  assert.deepEqual(cpuLines(result), [
    'release build: host_cpu cpu_count=2 counter_deltas=baseline',
    'release build: host_cpu index=0 user_ms=100 sys_ms=200 idle_ms=300 nice_ms=40 irq_ms=50',
    'release build: host_cpu index=1 user_ms=10 sys_ms=20 idle_ms=30 nice_ms=4 irq_ms=5',
    'release build: host_cpu cpu_count=2 counter_deltas=available',
    'release build: host_cpu index=0 user_ms=107 user_delta_ms=7 sys_ms=211 sys_delta_ms=11 idle_ms=313 idle_delta_ms=13 nice_ms=57 nice_delta_ms=17 irq_ms=69 irq_delta_ms=19',
    'release build: host_cpu index=1 user_ms=11 user_delta_ms=1 sys_ms=22 sys_delta_ms=2 idle_ms=33 idle_delta_ms=3 nice_ms=8 nice_delta_ms=4 irq_ms=10 irq_delta_ms=5',
  ]);
  assert.doesNotMatch(result.text(), /PRIVATE_|percent|busy|total_ms=/);
  assert.equal(result.timers[0].ms, 60000);
});

test('host CPU counters precede and survive each failed external Windows query', () => {
  const error = Object.assign(new Error('PRIVATE_QUERY'), { code: 'ETIMEDOUT' });
  const result = run([error, error], 'win32', [[cpu(1, 2, 3, 0, 8)], [cpu(4, 6, 9, 0, 12)]]);
  result.timers[0].callback();
  assert.match(cpuLines(result)[2], /counter_deltas=available$/);
  assert.match(cpuLines(result)[3], /sys_delta_ms=4.*irq_delta_ms=4/);
  assert.ok(result.logs.indexOf(cpuLines(result)[0]) < result.logs.findIndex(line => line.includes('ETIMEDOUT')));
  assert.equal(result.calls.length, 2);
  assert.doesNotMatch(result.text(), /PRIVATE_/);
});

test('CPU count changes and any per-core counter reset establish a new baseline', async t => {
  for (const [name, next] of [['count', [cpu(), cpu()]], ['counter', [cpu(99, 200, 300, 40, 50)]]]) {
    await t.test(name, () => {
      const result = run(['', '', ''], 'linux', [[cpu(100, 200, 300, 40, 50)], next, next]);
      result.timers[0].callback();
      const reset = cpuLines(result).findIndex(line => /counter_deltas=reset$/.test(line));
      assert.ok(reset > 0);
      assert.doesNotMatch(cpuLines(result)[reset + 1], /_delta_ms=/);
      result.timers[0].callback();
      assert.match(cpuLines(result).at(-1), /user_delta_ms=0/);
    });
  }
});

test('invalid or unavailable CPU data emits no partial metrics or raw errors and resets baseline', async t => {
  for (const [index, invalid] of [[], [cpu(-1)], [cpu(1.5)], [cpu(NaN)], [cpu(Infinity)],
    [cpu(Number.MAX_SAFE_INTEGER + 1)], [cpu(), {}], new Error('PRIVATE_CPU_ERROR')].entries()) {
    await t.test(String(index), () => {
      const result = run(['', '', ''], 'linux', [[cpu(10)], invalid, [cpu(20)]]);
      result.timers[0].callback();
      assert.deepEqual(cpuLines(result).slice(2), ['release build: host_cpu snapshot unavailable']);
      result.timers[0].callback();
      assert.match(cpuLines(result).at(-2), /counter_deltas=baseline$/);
      assert.doesNotMatch(cpuLines(result).at(-1), /_delta_ms=/);
      assert.doesNotMatch(result.text(), /PRIVATE_/);
      assert.equal(result.calls.length, 3);
    });
  }
});
