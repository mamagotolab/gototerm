param([Parameter(Mandatory = $true)][string]$Destination)
$ErrorActionPreference = 'Stop'

# portable-pty loads these siblings in preference to the OS ConPTY, which can
# replace VT scrolling with redraws and discard the information copy mode needs.
$version = '1.24.261001001'
$sha256 = '4d6aaddc1d2385c9f5897df28f33879f699f8f2783315d5204cf3d8c3616ac5f'
$work = Join-Path ([IO.Path]::GetTempPath()) "gototerm-conpty-$version"
New-Item -ItemType Directory -Force $work, $Destination | Out-Null
$archive = Join-Path $work 'conpty.zip'
Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/microsoft.windows.console.conpty.$version.nupkg" -OutFile $archive
if ((Get-FileHash $archive -Algorithm SHA256).Hash.ToLower() -ne $sha256) {
    throw 'Microsoft ConPTY archive checksum mismatch'
}
$extract = Join-Path $work 'package'
Expand-Archive -Path $archive -DestinationPath $extract -Force
Copy-Item (Join-Path $extract 'runtimes/win-x64/native/conpty.dll') $Destination -Force
Copy-Item (Join-Path $extract 'build/native/runtimes/x64/OpenConsole.exe') $Destination -Force
Copy-Item (Join-Path $PSScriptRoot '../../assets/licenses/ConPTY.txt') (Join-Path $Destination 'LICENSE-ConPTY.txt') -Force
