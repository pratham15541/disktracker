$ErrorActionPreference = 'Stop'

$packageName = 'disktracker'
$url64 = "https://github.com/pratham15541/disktracker/releases/download/__RELEASE_TAG__/disktracker-__RELEASE_TAG__-windows-x64.zip"
$checksum64 = '__CHECKSUM_X64__'

$packageArgs = @{
  packageName   = $packageName
  unzipLocation = "$(Split-Path -parent $MyInvocation.MyCommand.Definition)"
  url64bit      = $url64
  checksum64    = $checksum64
  checksumType64= 'sha256'
  fileType      = 'zip'
}

Install-ChocolateyZipPackage @packageArgs
