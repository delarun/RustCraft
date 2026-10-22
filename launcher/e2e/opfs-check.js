// Worker side of `node e2e/launcher.mjs fs`: checks src/jvm/opfs.ts against real OPFS.
const log = [];
const check = (name, ok, detail = '') => log.push((ok ? 'OK  ' : 'BAD ') + name + (detail ? ' ' + detail : ''));
try {
  const { OpfsBackend } = await import('/src/jvm/opfs.ts');
  const fs = new OpfsBackend('minecraft', { lazy: { dir: '/assets/objects', index: '/assets/indexes/34.json', url: '/proxy/resources.download.minecraft.net' } });
  let dir = await navigator.storage.getDirectory();
  for (const name of ['minecraft', 'assets', 'indexes']) dir = await dir.getDirectoryHandle(name);
  const index = JSON.parse(await (await (await dir.getFileHandle('34.json')).getFile()).text());
  const sound = index.objects['minecraft/sounds/random/click.ogg'];
  const path = '/assets/objects/' + sound.hash.slice(0, 2) + '/' + sound.hash;
  await fs.unlink(path).catch(() => {});
  const stat = await fs.stat(path);
  check('lazy stat before fetch', stat && stat.kind === 'file' && stat.size === sound.size, JSON.stringify(stat));
  check('lazy listing', (await fs.list('/assets/objects/' + sound.hash.slice(0, 2))).includes(sound.hash));
  const ro = { write: false, create: false, exclusive: false, truncate: false };
  const h = await fs.open(path, ro);
  const shared = new Uint8Array(new SharedArrayBuffer(sound.size));
  const n = h.read(0, shared);
  const head = new TextDecoder().decode(shared.slice(0, 4));
  check('lazy open fetched and read into shared memory', n === sound.size && head === 'OggS', n + ' ' + head);
  const h2 = await fs.open(path, ro);
  h.close();
  check('second descriptor shares the handle', h2.size() === sound.size);
  h2.close();
  const rw = { write: true, create: true, exclusive: false, truncate: true };
  await fs.mkdir('/e2e-test').catch(() => {});
  const w = await fs.open('/e2e-test/a.tmp', rw);
  w.write(0, new TextEncoder().encode('hello world'));
  check('write + size', w.size() === 11);
  await fs.rename('/e2e-test/a.tmp', '/e2e-test/b.txt');
  check('rename of an open file keeps it readable', w.size() === 11 && (await fs.stat('/e2e-test/a.tmp')) === null);
  w.write(11, new TextEncoder().encode('!'));
  w.close();
  const r = await fs.open('/e2e-test/b.txt', ro);
  const back = new Uint8Array(12); r.read(0, back); r.close();
  check('content after rename', new TextDecoder().decode(back) === 'hello world!');
  const x = await fs.open('/e2e-test/c.txt', rw); x.write(0, new Uint8Array([1])); x.close();
  await fs.rename('/e2e-test/c.txt', '/e2e-test/b.txt');
  check('rename replaces target', (await fs.stat('/e2e-test/b.txt')).size === 1);
  let err = null; try { await fs.open('/e2e-test/b.txt', { ...rw, exclusive: true }); } catch (e) { err = e.errno; }
  check('exclusive create of existing file fails EEXIST', err === 20, String(err));
  err = null; try { await fs.rmdir('/e2e-test'); } catch (e) { err = e.errno; }
  check('rmdir of non-empty dir fails ENOTEMPTY', err === 55, String(err));
  await fs.unlink('/e2e-test/b.txt');
  await fs.rmdir('/e2e-test');
  check('cleanup', (await fs.stat('/e2e-test')) === null);
  check('missing file', (await fs.stat('/nope/x')) === null);
  const opts = await fs.stat('/options.txt');
  check('game options persisted in OPFS', opts && opts.size > 0, JSON.stringify(opts));
} catch (e) { log.push('BAD exception ' + (e && e.stack || e)); }
postMessage(log);
