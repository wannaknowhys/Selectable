// Fetch PP-OCRv6 model triples into models/<tier>/ (gitignored).
// Zero dependencies: node builtins only (https/fs/path).
// Usage: node tools/fetch-models.mjs --tier small | --tier medium | --all
// The char dict ships inside each tier's inference.yml -> no separate dict download.
'use strict';

import fs from 'node:fs';
import path from 'node:path';
import https from 'node:https';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function readLock() {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  let raw = fs.readFileSync(path.join(dir, 'models.lock.json'), 'utf8');
  if (raw.charCodeAt(0) === 0xfeff) raw = raw.slice(1); // tolerate UTF-8 BOM editors
  return JSON.parse(raw);
}

function get(url, dest) {
  return new Promise((resolve, reject) => {
    const req = https.get(url, { headers: { 'User-Agent': 'selectable-fetch/0.1' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        // HF sometimes replies with a relative redirect (small git-tracked files)
        const next = new URL(res.headers.location, url).href;
        return get(next, dest).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        res.resume();
        return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      }
      const total = Number(res.headers['content-length'] || 0);
      let done = 0;
      const out = fs.createWriteStream(dest);
      res.on('data', (c) => {
        done += c.length;
        if (total) process.stdout.write(`\r  ${path.basename(dest)} ${(done / 1048576).toFixed(1)}/${(total / 1048576).toFixed(1)} MB`);
      });
      res.pipe(out);
      out.on('finish', () => { process.stdout.write('\n'); out.close(resolve); });
      out.on('error', reject);
    });
    req.on('error', reject);
  });
}

async function fetchTier(lock, tier) {
  const spec = lock.tiers[tier];
  if (!spec) throw new Error(`unknown tier: ${tier}`);
  const dir = path.join(ROOT, 'models', tier);
  fs.mkdirSync(dir, { recursive: true });
  const jobs = [
    ['det', 'det.onnx'],
    ['rec', 'rec.onnx'],
    ['yml', 'inference.yml'],
  ];
  for (const [key, name] of jobs) {
    const s = spec[key];
    const url = `${lock.hfBase}/${s.repo}/resolve/${s.rev}/${s.file}`;
    const dest = path.join(dir, name);
    if (fs.existsSync(dest) && s.bytes && fs.statSync(dest).size === s.bytes) {
      console.log(`  ${tier}/${name} already present, skipping`);
      continue;
    }
    console.log(`  downloading ${tier}/${name}`);
    await get(url, dest);
    if (s.bytes) {
      const got = fs.statSync(dest).size;
      // repo totals include yml/readme; onnx must simply be plausible (>50% of total)
      if (got < s.bytes * 0.5) throw new Error(`suspicious size for ${tier}/${name}: ${got} bytes`);
    }
  }
  console.log(`tier ${tier} complete -> ${dir}`);
}

async function main() {
  const args = process.argv.slice(2);
  const lock = readLock();
  let tiers = [];
  if (args.includes('--all')) tiers = Object.keys(lock.tiers);
  else {
    const i = args.indexOf('--tier');
    if (i === -1 || !args[i + 1]) throw new Error('usage: fetch-models.mjs --tier small|medium | --all');
    tiers = [args[i + 1]];
  }
  for (const t of tiers) await fetchTier(lock, t);
}

main().catch((e) => { console.error('fetch failed:', e.message); process.exit(1); });
