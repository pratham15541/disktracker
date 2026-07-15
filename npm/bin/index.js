#!/usr/bin/env node
const { spawn } = require('child_process');
const path = require('path');

const binaryPath = path.join(__dirname, 'disktracker.exe');
const args = process.argv.slice(2);

const child = spawn(binaryPath, args, { stdio: 'inherit' });

child.on('close', (code) => {
  process.exit(code !== null ? code : 1);
});

child.on('error', (err) => {
  console.error('Failed to start disktracker CLI:', err);
  process.exit(1);
});
