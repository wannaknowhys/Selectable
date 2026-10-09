// Fetch PP-OCRv6 model triples into models/<tier>/ (gitignored).
// Zero dependencies: node builtins only (https/fs/path).
// Usage: node tools/fetch-models.mjs --tier small | --tier medium | --all
// The char dict ships inside each tier's inference.yml -> no separate dict download.
'use strict';

import fs from 'node:fs';
import path from 'node:path';
import https from 'node:https';
import zlib from 'node:zlib';
import crypto from 'node:crypto';
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
    ['det_yml', 'det.yml'],
    ['rec', 'rec.onnx'],
    ['rec_yml', 'rec.yml'],
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
  if (args.includes('--translate')) {
    // --translate [pair...]: default enzh+zhen from the pinned lock entries.
    const i = args.indexOf('--translate');
    const rest = args.slice(i + 1).filter((a) => !a.startsWith('--'));
    const pairs = rest.length > 0 ? rest : Object.keys(lock.translate.pinned);
    for (const p of pairs) await fetchTranslatePair(lock, p);
    return;
  }
  let tiers = [];
  if (args.includes('--all')) tiers = Object.keys(lock.tiers);
  else {
    const i = args.indexOf('--tier');
    if (i === -1 || !args[i + 1]) throw new Error('usage: fetch-models.mjs --tier small|medium | --all');
    tiers = [args[i + 1]];
  }
  for (const t of tiers) await fetchTier(lock, t);
}

function getBuffer(url) {
  return new Promise((resolve, reject) => {
    const req = https.get(url, { headers: { 'User-Agent': 'selectable-fetch/0.1' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        getBuffer(new URL(res.headers.location, url).href).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        res.resume();
        reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        return;
      }
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => resolve(Buffer.concat(chunks)));
    });
    req.on('error', reject);
  });
}

// Translation pair fetch: pinned registry files -> models/translate/{pair}/
// plus a bergamot config.yml (single-vocab models list it twice).
async function fetchTranslatePair(lock, pair) {
  const spec = lock.translate.pinned[pair];
  if (!spec) throw new Error(`unknown translate pair: ${pair}`);
  const base = 'https://storage.googleapis.com/moz-fx-translations-data--303e-prod-translations-data';
  const dir = path.join(ROOT, 'models', 'translate', pair);
  fs.mkdirSync(dir, { recursive: true });
  const names = {};
  for (const [key, f] of Object.entries(spec.files)) {
    const base_name = path.basename(f.path).replace(/\.gz$/, '');
    const dest = path.join(dir, base_name);
    names[key] = base_name;
    if (fs.existsSync(dest)) {
      console.log(`  ${pair}/${base_name} already present, skipping`);
      continue;
    }
    console.log(`  downloading ${pair}/${base_name}`);
    const data = await getBuffer(`${base}/${f.path}`);
    const raw = f.path.endsWith('.gz') ? zlib.gunzipSync(data) : data;
    if (f.uncompressedSize && raw.length !== f.uncompressedSize) {
      throw new Error(`size mismatch for ${pair}/${base_name}: ${raw.length} != ${f.uncompressedSize}`);
    }
    if (f.uncompressedHash) {
      const hash = crypto.createHash('sha256').update(raw).digest('hex');
      if (hash !== f.uncompressedHash) throw new Error(`sha256 mismatch for ${pair}/${base_name}`);
    }
    fs.writeFileSync(dest, raw);
  }
  const vocabs = names.vocab
    ? [`    - ${names.vocab}`, `    - ${names.vocab}`]
    : [`    - ${names.srcVocab}`, `    - ${names.trgVocab}`];
  const cfg = [
    'relative-paths: true',
    'models:',
    `  - ${names.model}`,
    'vocabs:',
    ...vocabs,
    'shortlist:',
    `  - ${names.lexicalShortlist}`,
    '  - false',
    'beam-size: 1',
    'normalize: 1.0',
    'word-penalty: 0',
    'max-length-break: 128',
    'mini-batch-words: 1024',
    'workspace: 128',
    'max-length-factor: 2.0',
    'skip-cost: true',
    'cpu-threads: 4',
    'quiet: true',
    'quiet-translation: true',
    'gemm-precision: int8shiftAlphaAll',
    '',
  ].join('\n');
  fs.writeFileSync(path.join(dir, 'config.yml'), cfg);
  console.log(`translate ${pair} complete -> ${dir}`);
}

main().catch((e) => { console.error('fetch failed:', e.message); process.exit(1); });
