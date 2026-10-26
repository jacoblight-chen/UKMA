#!/usr/bin/env node
// The npx/npm launcher for UKMA.
//
// This file contains no code-graph logic: it locates the platform binary
// (downloading it from the GitHub release and verifying its SHA-256 on first
// use) and then replaces itself with it, forwarding the exit code. Everything
// that decides what UKMA does lives in the Rust binary.
//
// `npx ukma-cli` with no arguments runs `ukma install`, matching CodeGraph's
// "downloads and runs the installer in one go".

const { spawnSync } = require('node:child_process');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const https = require('node:https');
const os = require('node:os');
const path = require('node:path');

const REPO = 'jacoblight-chen/UKMA';
const TRIPLES = {
  'win32-x64': 'x86_64-pc-windows-msvc',
  'win32-arm64': 'aarch64-pc-windows-msvc',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'darwin-x64': 'x86_64-apple-darwin',
  'darwin-arm64': 'aarch64-apple-darwin',
};

function fail(message) {
  process.stderr.write(`ukma: ${message}\n`);
  process.exit(1);
}

function targetTriple() {
  const triple = TRIPLES[`${process.platform}-${process.arch}`];
  if (!triple) {
    fail(
      `no UKMA build is published for ${process.platform}/${process.arch}; ` +
        'build it from source (cargo build -p ukma-cli) or set UKMA_BIN',
    );
  }
  return triple;
}

function packageVersion() {
  return require(path.join(__dirname, '..', 'package.json')).version;
}

function binaryDir() {
  return process.env.UKMA_BIN_DIR || path.join(os.homedir(), '.ukma', 'bin');
}

function binaryPath() {
  const name = process.platform === 'win32' ? 'ukma.exe' : 'ukma';
  return path.join(binaryDir(), name);
}

function assetUrl(triple) {
  const version = packageVersion();
  const name = process.platform === 'win32' ? `ukma-${triple}.exe` : `ukma-${triple}`;
  // A dev version is not published, so fall back to the rolling `latest` tag.
  const base = /^\d+\.\d+\.\d+$/.test(version)
    ? `https://github.com/${REPO}/releases/download/v${version}`
    : `https://github.com/${REPO}/releases/latest/download`;
  return { url: `${base}/${name}`, name };
}

function download(url, destination, redirects = 5) {
  return new Promise((resolve, reject) => {
    const request = https.get(url, { headers: { 'user-agent': 'ukma-cli-npm' } }, (response) => {
      const status = response.statusCode || 0;
      if (status >= 300 && status < 400 && response.headers.location) {
        response.resume();
        if (redirects <= 0) {
          reject(new Error(`too many redirects for ${url}`));
          return;
        }
        download(new URL(response.headers.location, url).toString(), destination, redirects - 1)
          .then(resolve, reject);
        return;
      }
      if (status !== 200) {
        response.resume();
        reject(new Error(`${url} returned HTTP ${status}`));
        return;
      }
      const file = fs.createWriteStream(destination);
      response.pipe(file);
      file.on('finish', () => file.close(() => resolve()));
      file.on('error', reject);
    });
    request.on('error', reject);
    request.setTimeout(300000, () => request.destroy(new Error(`timeout downloading ${url}`)));
  });
}

async function sha256(file) {
  const hash = createHash('sha256');
  await new Promise((resolve, reject) => {
    const stream = fs.createReadStream(file);
    stream.on('data', (chunk) => hash.update(chunk));
    stream.on('end', resolve);
    stream.on('error', reject);
  });
  return hash.digest('hex');
}

async function ensureBinary() {
  if (process.env.UKMA_BIN) {
    return process.env.UKMA_BIN;
  }
  const destination = binaryPath();
  if (fs.existsSync(destination)) {
    return destination;
  }
  const triple = targetTriple();
  const { url, name } = assetUrl(triple);
  const directory = path.dirname(destination);
  const rootDirectory = path.dirname(directory);
  const created = !fs.existsSync(directory);
  const createdRoot = !fs.existsSync(rootDirectory);
  fs.mkdirSync(directory, { recursive: true });
  const temporary = `${destination}.download`;
  const cleanup = () => {
    // A failed download must not leave a partial binary (or the directories this
    // run created) behind for the next attempt to trip over.
    for (const leftover of [temporary, `${temporary}.sha256`]) {
      fs.rmSync(leftover, { force: true });
    }
    for (const [ours, target] of [
      [created, directory],
      [createdRoot, rootDirectory],
    ]) {
      if (!ours) continue;
      try {
        if (fs.readdirSync(target).length === 0) {
          fs.rmdirSync(target);
        }
      } catch {
        /* the directory is in use; leaving it is harmless */
      }
    }
  };
  process.stderr.write(`ukma: downloading ${name} for ${triple}\n`);

  let expected = null;
  try {
    const digestFile = `${temporary}.sha256`;
    await download(`${url}.sha256`, digestFile);
    expected = fs.readFileSync(digestFile, 'utf8').trim().split(/\s+/)[0].toLowerCase();
    fs.rmSync(digestFile, { force: true });
  } catch (error) {
    process.stderr.write(`ukma: no published SHA-256 (${error.message}); continuing\n`);
  }

  try {
    await download(url, temporary);
    if (expected) {
      const actual = await sha256(temporary);
      if (actual !== expected) {
        cleanup();
        fail(`SHA-256 mismatch for ${name}: expected ${expected}, got ${actual}`);
      }
    }
    if (process.platform !== 'win32') {
      fs.chmodSync(temporary, 0o755);
    }
    fs.renameSync(temporary, destination);
  } catch (error) {
    cleanup();
    throw error;
  }
  return destination;
}

async function main() {
  const binary = await ensureBinary();
  const args = process.argv.slice(2);
  // CodeGraph parity: `npx <package>` with no arguments runs the install step.
  const forwarded = args.length === 0 ? ['install'] : args;
  const result = spawnSync(binary, forwarded, { stdio: 'inherit' });
  if (result.error) {
    fail(`cannot run ${binary} (${result.error.message})`);
  }
  process.exit(result.status === null ? 1 : result.status);
}

main().catch((error) => fail(error.message));
