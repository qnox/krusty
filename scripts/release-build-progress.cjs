// Live release-build diagnostics while Cargo is quiet during code generation or linking.
// Report process names and resource use only; arguments and environment may contain secrets.
const os = require('node:os');
const { execFileSync } = require('node:child_process');

const started = Date.now();
const mib = bytes => Math.round(bytes / 1024 / 1024);
console.error(`release build: ${os.platform()} ${os.arch()}, ${os.cpus().length} CPUs, ${mib(os.totalmem())} MiB RAM`);

function tasklistError(kind) {
  return Object.assign(new Error('invalid tasklist response'), { code: `TASKLIST_${kind}` });
}

function csvRows(text) {
  const rows = [];
  let row = [], field = '', quoted = false, closed = false;
  for (let index = 0; index <= text.length; index++) {
    const character = text[index];
    if (quoted) {
      if (character === '"') {
        if (text[index + 1] === '"') { field += '"'; index++; }
        else { quoted = false; closed = true; }
      } else if (character === undefined) throw tasklistError('CSV');
      else field += character;
    } else if (character === ',' || character === '\n' || character === undefined) {
      row.push(field); field = ''; closed = false;
      if (character !== ',') {
        if (row.some(value => value.length)) rows.push(row);
        row = [];
      }
    } else if (character === '\r' && text[index + 1] === '\n') {
      // A record separator outside quotes; embedded newlines remain part of their field.
    } else if (character === '"' && !field && !closed) quoted = true;
    else {
      if (closed || character === '"' || character === '\r') throw tasklistError('CSV');
      field += character;
    }
  }
  return rows;
}

function tasklistProcesses(text) {
  const rows = csvRows(text.replace(/^\uFEFF/, ''));
  const headers = rows.shift()?.map(value => value.toLowerCase().replace(/\s/g, ''));
  const columns = ['imagename', 'pid', 'memusage', 'cputime'].map(name => {
    if (!headers || headers.filter(value => value === name).length !== 1) throw tasklistError('SCHEMA');
    return headers.indexOf(name);
  });
  const selected = new Set(['cargo.exe', 'rustc.exe', 'link.exe', 'lld-link.exe', 'clang.exe', 'clang-cl.exe']);
  const processes = [];
  for (const row of rows) {
    if (row.length !== headers.length) throw tasklistError('SCHEMA');
    const image = row[columns[0]].toLowerCase();
    if (!selected.has(image)) continue;
    const pidText = row[columns[1]], pid = Number(pidText);
    const memory = row[columns[2]].match(/^(\d+|\d{1,3}([,.\u00a0\u202f ])\d{3}(?:\2\d{3})*)\s+K$/i);
    const cpu = row[columns[3]].match(/^(\d+):([0-5]\d):([0-5]\d)$/);
    const kib = memory && Number(memory[1].replace(/[,.\s]/g, ''));
    const seconds = cpu && Number(cpu[1]) * 3600 + Number(cpu[2]) * 60 + Number(cpu[3]);
    if (!/^\d+$/.test(pidText) || !Number.isSafeInteger(pid) || pid <= 0
      || !Number.isSafeInteger(kib) || kib < 0 || !Number.isSafeInteger(seconds)) throw tasklistError('DATA');
    processes.push(`${pid} ${image} ${seconds} ${Math.round(kib / 1024)}`);
  }
  return processes;
}


const vmStatPages = {
  'Pages free': 'free_pages', 'Pages active': 'active_pages',
  'Pages inactive': 'inactive_pages', 'Pages speculative': 'speculative_pages',
  'Pages wired down': 'wired_pages', 'Pages purgeable': 'purgeable_pages',
  'Pages occupied by compressor': 'compressor_pages',
};
const vmStatCounters = {
  Pageouts: 'pageouts', Swapouts: 'swapouts',
  Compressions: 'compressions', Decompressions: 'decompressions',
};
let previousVmStat;

function vmStatSnapshot(text) {
  const invalid = () => Object.assign(new Error('invalid vm_stat response'), { code: 'VMSTAT_DATA' });
  const lines = text.trim().split(/\r?\n/);
  const header = lines.shift().match(/^Mach Virtual Memory Statistics: \(page size of (\d+) bytes\)$/);
  const pageSize = header && Number(header[1]);
  if (!Number.isSafeInteger(pageSize) || pageSize <= 0) throw invalid();
  const fields = { ...vmStatPages, ...vmStatCounters }, values = {};
  for (const line of lines) {
    const colon = line.indexOf(':'), name = line.slice(0, colon);
    if (!Object.hasOwn(fields, name)) continue;
    const value = line.slice(colon + 1).trim().match(/^(\d+)\.$/);
    const count = value && Number(value[1]);
    if (!Number.isSafeInteger(count) || count < 0 || Object.hasOwn(values, name)) throw invalid();
    values[name] = count;
  }
  if (Object.keys(values).length !== Object.keys(fields).length) throw invalid();
  return { pageSize, values };
}

function reportDarwinMemory() {
  try {
    const sample = vmStatSnapshot(execFileSync('vm_stat', [], {
      encoding: 'utf8', timeout: 15_000, maxBuffer: 64 * 1024, stdio: ['ignore', 'pipe', 'pipe'],
    }));
    const comparable = previousVmStat && previousVmStat.pageSize === sample.pageSize
      && Object.keys(vmStatCounters).every(name => sample.values[name] >= previousVmStat.values[name]);
    const values = [`page_size_bytes=${sample.pageSize}`];
    for (const [name, key] of Object.entries(vmStatPages)) values.push(`${key}=${sample.values[name]}`);
    // Native counters count pages since boot; deltas cover consecutive valid snapshots.
    for (const [name, key] of Object.entries(vmStatCounters)) {
      values.push(`${key}_pages_total=${sample.values[name]}`);
      if (comparable) values.push(`${key}_pages_delta=${sample.values[name] - previousVmStat.values[name]}`);
    }
    values.push(`counter_deltas=${comparable ? 'available' : previousVmStat ? 'reset' : 'baseline'}`);
    console.error(`release build: vm_stat ${values.join(' ')}`);
    previousVmStat = sample;
  } catch (error) {
    previousVmStat = undefined;
    const code = /^[A-Z][A-Z0-9_]{0,31}$/.test(error.code) ? error.code
      : Number.isSafeInteger(error.status) ? error.status : 'ERROR';
    console.error(`release build: vm_stat snapshot unavailable (${code})`);
  }
}

function report() {
  console.error(`release build: ${new Date().toISOString()}, elapsed ${Math.round((Date.now() - started) / 1000)}s, free RAM ${mib(os.freemem())}/${mib(os.totalmem())} MiB`);
  try {
    const options = { encoding: 'utf8', timeout: 15_000, stdio: ['ignore', 'pipe', 'pipe'] };
    if (os.platform() === 'win32') {
      console.error('release build: process CPU is cumulative seconds; tasklist memory is MiB');
      // Keep headers to authenticate field positions; verbose CSV also contains private columns.
      const processes = tasklistProcesses(execFileSync('tasklist.exe', ['/V', '/FO', 'CSV'], options));
      console.error('release build: PID IMAGE CPU_SECONDS MEMORY_MiB');
      for (const process of processes) console.error(process);
      if (!processes.length) console.error('release build: no compiler or linker processes present');
    } else {
      const processes = execFileSync('ps', ['-A', '-o', 'pid=,ppid=,%cpu=,rss=,etime=,comm='], options);
      console.error('release build: PID PPID CPU% RSS_KiB ELAPSED PROCESS');
      for (const line of processes.split('\n')) {
        if (/(?:^|[/\s])(?:cargo|rustc|ld|ld64|lld|ld.lld|clang|clang-cl|cc1)$/.test(line.trim())) {
          console.error(line.trim());
        }
      }
    }
  } catch (error) {
    console.error(`release build: resource snapshot unavailable (${error.code || error.status || error.name})`);
    // A failed native query can carry partial rows containing usernames and window titles.
    const detail = os.platform() === 'win32' ? undefined : error.stderr?.toString().trim();
    if (detail) console.error(detail.slice(0, 2000));
  }
  if (os.platform() === 'darwin') reportDarwinMemory();
}

report();
setInterval(report, 60_000);
