#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const http = require("http");
const { URL } = require("url");
const tar = require("tar");
const extractZip = require("extract-zip");

const repo = process.env.DISKTRACKER_REPO || "pratham15541/disktracker";
const pkg = require("./package.json");

const rawVersion = process.env.DISKTRACKER_VERSION || pkg.version;
if (!rawVersion || rawVersion === "0.0.0") {
  console.error(
    "Missing version. Set DISKTRACKER_VERSION or publish with a real package version."
  );
  process.exit(1);
}

const tag = rawVersion.startsWith("v") ? rawVersion : `v${rawVersion}`;

const platformMap = {
  linux: "linux",
  darwin: "macos",
  win32: "windows",
};

const archMap = {
  x64: "x64",
  arm64: "arm64",
};

const platform = platformMap[process.platform];
const arch = archMap[process.arch];
const supportedPlatforms = Object.keys(platformMap).join(", ");
const supportedArchs = Object.keys(archMap).join(", ");

if (!platform) {
  console.error(
    `Unsupported platform: ${process.platform}. Supported: ${supportedPlatforms}.`
  );
  process.exit(1);
}

if (!arch) {
  console.error(
    `Unsupported architecture: ${process.arch}. Supported: ${supportedArchs}.`
  );
  process.exit(1);
}

const ext = platform === "windows" ? "zip" : "tar.gz";
const asset = `disktracker-${tag}-${platform}-${arch}.${ext}`;
const url = `https://github.com/${repo}/releases/download/${tag}/${asset}`;

function downloadFile(fileUrl, destination) {
  return new Promise((resolve, reject) => {
    const tryDownload = (currentUrl, redirects) => {
      const client = currentUrl.startsWith("https:") ? https : http;
      const request = client.get(currentUrl, (res) => {
        if (
          res.statusCode &&
          res.statusCode >= 300 &&
          res.statusCode < 400 &&
          res.headers.location
        ) {
          if (redirects > 5) {
            reject(new Error("Too many redirects while downloading asset."));
            return;
          }
          const nextUrl = new URL(res.headers.location, currentUrl).toString();
          res.resume();
          tryDownload(nextUrl, redirects + 1);
          return;
        }

        if (res.statusCode !== 200) {
          reject(
            new Error(
              `Download failed with status ${res.statusCode} for ${currentUrl}`
            )
          );
          res.resume();
          return;
        }

        const file = fs.createWriteStream(destination);
        res.pipe(file);
        file.on("finish", () => file.close(resolve));
        file.on("error", reject);
      });

      request.on("error", reject);
    };

    tryDownload(fileUrl, 0);
  });
}

async function extractArchive(archivePath, destinationDir, archiveExt) {
  if (archiveExt === "zip") {
    await extractZip(archivePath, { dir: destinationDir });
    return;
  }

  await tar.x({ file: archivePath, cwd: destinationDir });
}

async function install() {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "disktracker-"));
  const archivePath = path.join(tmpDir, asset);

  try {
    await downloadFile(url, archivePath);
    await extractArchive(archivePath, tmpDir, ext);

    const binName = platform === "windows" ? "disktracker.exe" : "disktracker";
    const extractedBin = path.join(tmpDir, binName);

    if (!fs.existsSync(extractedBin)) {
      throw new Error("Downloaded archive does not contain the disktracker binary.");
    }

    const nativeDir = path.join(__dirname, "native");
    fs.mkdirSync(nativeDir, { recursive: true });

    const targetBin = path.join(nativeDir, binName);
    fs.copyFileSync(extractedBin, targetBin);

    if (platform !== "windows") {
      fs.chmodSync(targetBin, 0o755);
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

install().catch((error) => {
  console.error("Failed to install disktracker:", error.message);
  process.exit(1);
});
