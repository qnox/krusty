// Live release-build diagnostics while Cargo is quiet during code generation or linking.
// Report process names and resource use only; arguments and environment may contain secrets.
const os = require('node:os');
const { execFileSync } = require('node:child_process');

const started = Date.now();
const mib = bytes => Math.round(bytes / 1024 / 1024);
console.error(`release build: ${os.platform()} ${os.arch()}, ${os.cpus().length} CPUs, ${mib(os.totalmem())} MiB RAM`);

function report() {
  console.error(`release build: ${new Date().toISOString()}, elapsed ${Math.round((Date.now() - started) / 1000)}s, free RAM ${mib(os.freemem())}/${mib(os.totalmem())} MiB`);
  try {
    const options = { encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'ignore'] };
    if (os.platform() === 'win32') {
      console.error('release build: process CPU is cumulative seconds; working set is MiB');
      console.error(execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
        'Get-Process -Name cargo,rustc,link,lld-link,clang,clang-cl -ErrorAction SilentlyContinue | Select-Object Id,ProcessName,CPU,@{Name="WorkingSetMiB";Expression={[math]::Round($_.WorkingSet64 / 1MB)}} | Format-Table -AutoSize | Out-String'
      ], options).trim());
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
  }
}

report();
setInterval(report, 60_000);
