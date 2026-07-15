const fs = require('fs');
const https = require('https');
const { execSync } = require('child_process');
const path = require('path');

// Only run on Windows
if (process.platform !== 'win32') {
  console.error('Error: DiskTracker is only supported on Windows.');
  process.exit(1);
}

const pkg = require('./package.json');
const version = pkg.version;
const tag = `v${version}`;
const zipName = `disktracker-${tag}-windows-x64.zip`;
const url = `https://github.com/pratham15541/disktracker/releases/download/${tag}/${zipName}`;

const binDir = path.join(__dirname, 'bin');
if (!fs.existsSync(binDir)) {
  fs.mkdirSync(binDir, { recursive: true });
}

const zipPath = path.join(binDir, zipName);

console.log(`Downloading DiskTracker ${tag} from GitHub Releases...`);
console.log(`URL: ${url}`);

function download(downloadUrl, destPath) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(destPath);
    const request = (targetUrl) => {
      https.get(targetUrl, (response) => {
        if (response.statusCode >= 300 && response.statusCode < 400 && response.headers.location) {
          request(response.headers.location);
          return;
        }
        if (response.statusCode !== 200) {
          reject(new Error(`Failed to download (HTTP status code ${response.statusCode})`));
          return;
        }
        response.pipe(file);
        file.on('finish', () => {
          file.close(resolve);
        });
      }).on('error', (err) => {
        fs.unlink(destPath, () => reject(err));
      });
    };
    request(downloadUrl);
  });
}

download(url, zipPath)
  .then(() => {
    console.log('Extracting archive...');
    try {
      // Run PowerShell Expand-Archive to extract the zip file
      execSync(`powershell -NoProfile -Command "Expand-Archive -Path '${zipPath}' -DestinationPath '${binDir}' -Force"`, { stdio: 'inherit' });
      console.log('Extraction complete.');
    } catch (err) {
      console.error('Extraction failed:', err);
      process.exit(1);
    } finally {
      if (fs.existsSync(zipPath)) {
        fs.unlinkSync(zipPath);
      }
    }
  })
  .catch((err) => {
    console.error('Download failed:', err);
    process.exit(1);
  });
