// Summarizes CPU profiles from jvm.mjs CPU_PROFILE: per worker, the busiest
// functions by self time.
//   node e2e/profile-summary.mjs <dir> [top]
import { readdirSync, readFileSync } from 'node:fs';

const [dir, top = '20'] = process.argv.slice(2);
// Blocking calls (the thread waits in memory.atomic.wait or the host).
const WAITING = /Condvar\d*wait|sleep_nanos|poll_oneoff|lock_contended|futex_wait|park|clock_nanosleep|^\(idle\)$/;
const results = [];
for (const file of readdirSync(dir).filter((f) => f.endsWith('.cpuprofile'))) {
  const profile = JSON.parse(readFileSync(`${dir}/${file}`, 'utf8'));
  const byId = new Map(profile.nodes.map((n) => [n.id, n]));
  const counts = new Map();
  for (const id of profile.samples) counts.set(id, (counts.get(id) ?? 0) + 1);
  const self = new Map();
  let total = 0;
  let idle = 0;
  for (const [id, n] of counts) {
    const name = byId.get(id).callFrame.functionName || '(anonymous)';
    total += n;
    if (WAITING.test(name)) idle += n;
    self.set(name, (self.get(name) ?? 0) + n);
  }
  results.push({ file, total, idle, self });
}
results.sort((a, b) => b.total - b.idle - (a.total - a.idle));
for (const { file, total, idle, self } of results) {
  if (total - idle < total * 0.05) continue;
  console.log(`== ${file}: ${total} samples, ${(((total - idle) / total) * 100).toFixed(0)}% busy`);
  for (const [name, n] of [...self].sort((a, b) => b[1] - a[1]).slice(0, Number(top))) {
    console.log(`  ${((n / total) * 100).toFixed(1).padStart(5)}%  ${name.slice(0, 150)}`);
  }
}
