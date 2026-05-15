#!/usr/bin/env node
"use strict";

const path = require("path");
const { spawn } = require("child_process");

const binName = process.platform === "win32" ? "disktracker.exe" : "disktracker";
const binPath = path.join(__dirname, "..", "native", binName);

const child = spawn(binPath, process.argv.slice(2), { stdio: "inherit" });

child.on("error", (error) => {
  console.error("Failed to run disktracker:", error.message);
  process.exit(1);
});

child.on("exit", (code) => {
  process.exit(code ?? 1);
});
