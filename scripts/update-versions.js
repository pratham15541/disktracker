const fs = require('fs');
const path = require('path');

const cliCargoPath = path.join(__dirname, '../apps/cli/Cargo.toml');
if (!fs.existsSync(cliCargoPath)) {
  console.error(`Error: Cargo.toml not found at ${cliCargoPath}`);
  process.exit(1);
}

const cargoContent = fs.readFileSync(cliCargoPath, 'utf8');

// Match version under [package] section
const packageSection = cargoContent.split('[dependencies]')[0];
const versionMatch = packageSection.match(/version\s*=\s*"([^"]+)"/);
if (!versionMatch) {
  console.error("Could not find package version in apps/cli/Cargo.toml");
  process.exit(1);
}
const version = versionMatch[1];
console.log(`Detected DiskTracker CLI version: ${version}`);

// 1. Update npm/package.json
const npmPackageJsonPath = path.join(__dirname, '../npm/package.json');
if (fs.existsSync(npmPackageJsonPath)) {
  const pkg = JSON.parse(fs.readFileSync(npmPackageJsonPath, 'utf8'));
  pkg.version = version;
  fs.writeFileSync(npmPackageJsonPath, JSON.stringify(pkg, null, 2) + '\n');
  console.log(`Updated npm/package.json to version ${version}`);
} else {
  console.log(`Note: npm/package.json not found at ${npmPackageJsonPath}`);
}

// 2. Update chocolatey/disktracker.nuspec
const nuspecPath = path.join(__dirname, '../chocolatey/disktracker.nuspec');
if (fs.existsSync(nuspecPath)) {
  let nuspecContent = fs.readFileSync(nuspecPath, 'utf8');
  nuspecContent = nuspecContent.replace(/<version>[^<]+<\/version>/, `<version>${version}</version>`);
  fs.writeFileSync(nuspecPath, nuspecContent);
  console.log(`Updated chocolatey/disktracker.nuspec to version ${version}`);
} else {
  console.log(`Note: chocolatey/disktracker.nuspec not found at ${nuspecPath}`);
}
