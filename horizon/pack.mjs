// Writes the files horizon-prepare needs from Horizon's tree (its --root) as one pack for
// the launcher (launcher/src/jvm/pack.worker.ts): per file a u32 path length, the UTF-8 path,
// a u32 size and the bytes, little-endian. Prints the pack's sha256, its tag.
//   node horizon/pack.mjs <horizon checkout> <compiled exporters dir> <out.pack>

import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const [checkout, exporters, out] = process.argv.slice(2);
if (!out) {
  console.error('usage: node pack.mjs <horizon checkout> <compiled exporters dir> <out.pack>');
  process.exit(2);
}

/** Every file under `dir`, sorted, as [path in the pack, absolute path]. */
function tree(dir, prefix) {
  const files = [];
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) files.push(...tree(path, `${prefix}/${name}`));
    else files.push([`${prefix}/${name}`, path]);
  }
  return files;
}

const PARITY = 'mods/horizon/src/test/java/dev/horizonlab/parity';
const files = [
  ['research-lock.json', join(checkout, 'research-lock.json')],
  ['crates/horizon-prepare/src/exporters.rs', join(checkout, 'crates/horizon-prepare/src/exporters.rs')],
  ...tree(join(checkout, PARITY), PARITY),
  // The bundle horizon-prepare compile-exporters made: its manifest and classes (no logs).
  ['exporters/manifest.json', join(exporters, 'manifest.json')],
  ...tree(join(exporters, 'server'), 'exporters/server'),
  ...tree(join(exporters, 'client'), 'exporters/client'),
];

const parts = [];
for (const [name, path] of files) {
  const data = readFileSync(path);
  const encoded = Buffer.from(name.split('\\').join('/'));
  const header = Buffer.alloc(4);
  header.writeUInt32LE(encoded.length);
  const size = Buffer.alloc(4);
  size.writeUInt32LE(data.length);
  parts.push(header, encoded, size, data);
}
const pack = Buffer.concat(parts);
writeFileSync(out, pack);
console.log(createHash('sha256').update(pack).digest('hex'));
console.error(`${relative(process.cwd(), out)}: ${files.length} files, ${(pack.length / 1024).toFixed(0)} KiB`);
