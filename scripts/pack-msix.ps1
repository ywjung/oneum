# Packs the folder scripts/package-windows-store.sh made into the package for the
# Microsoft Store (MSIX). Runs on Windows, in PowerShell 7:
#
#   pwsh scripts/pack-msix.ps1 -Layout <the folder> -Package <the .msix to write>
#
# Needs the Windows SDK, for its two tools: MakePri.exe writes the index of the
# pictures (resources.pri: without it Windows does not find the pictures for a size, a
# scale or a theme, such as the taskbar's), MakeAppx.exe packs the folder and checks
# the manifest while it does. GitHub's Windows runners have the SDK.
#
# Nothing is signed: the Store signs a package it has accepted. The folder is not
# changed (the index is added to a copy). docs/MICROSOFT_STORE.md.
param(
    [Parameter(Mandatory = $true)][string]$Layout,
    [Parameter(Mandatory = $true)][string]$Package
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The newest copy of a tool of the Windows SDK, for 64-bit Windows.
function Find-SdkTool([string]$Name) {
    $root = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $found = @(Get-ChildItem -Path $root -Filter $Name -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' -and $_.Directory.Parent.Name -match '^\d+(\.\d+){3}$' } |
        Sort-Object -Property { [version]$_.Directory.Parent.Name })
    if ($found.Count -eq 0) {
        throw "$Name was not found under $root (is the Windows SDK installed?)"
    }
    return $found[-1].FullName
}

# Runs a tool and stops when it fails (PowerShell does not, by itself).
function Invoke-Tool([string]$Tool, [string[]]$Arguments) {
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$(Split-Path -Leaf $Tool) $($Arguments[0]) ended with $LASTEXITCODE"
    }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
}

$Layout = (Resolve-Path -LiteralPath $Layout).Path
$Package = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Package)
if (-not (Test-Path -LiteralPath (Join-Path $Layout 'AppxManifest.xml'))) {
    throw "$Layout has no AppxManifest.xml"
}
$makepri = Find-SdkTool 'makepri.exe'
$makeappx = Find-SdkTool 'makeappx.exe'
Write-Host "MakePri:  $makepri"
Write-Host "MakeAppx: $makeappx"

$work = Join-Path ([System.IO.Path]::GetTempPath()) ('oneum-msix-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $stage = Join-Path $work 'package'
    Copy-Item -LiteralPath $Layout -Destination $stage -Recurse
    $manifest = Join-Path $stage 'AppxManifest.xml'

    # The index of the pictures. MakePri's own configuration would split the index by
    # language and by scale into further files, which is for a set of packages; this
    # is one package, so that part is taken out.
    $config = Join-Path $work 'priconfig.xml'
    Invoke-Tool $makepri @('createconfig', '/cf', $config, '/dq', 'en-US', '/pv', '10.0.0', '/o')
    $xml = New-Object System.Xml.XmlDocument
    $xml.Load($config)
    foreach ($node in @($xml.SelectNodes('//packaging'))) {
        [void]$node.ParentNode.RemoveChild($node)
    }
    $xml.Save($config)
    $priFolder = Join-Path $work 'pri'
    New-Item -ItemType Directory -Path $priFolder | Out-Null
    $pri = Join-Path $priFolder 'resources.pri'
    Invoke-Tool $makepri @('new', '/pr', $stage, '/cf', $config, '/mn', $manifest, '/of', $pri, '/o')
    $made = @(Get-ChildItem -Path $work -Filter '*.pri' -Recurse)
    if ($made.Count -ne 1) {
        $names = ($made | ForEach-Object { $_.Name }) -join ', '
        throw "MakePri wrote $($made.Count) index files, not one: $names"
    }

    # The index knows every picture as a form of the one the manifest names: read it
    # back and count. (A picture whose name MakePri did not understand would be a
    # resource of its own, and Windows would never choose it.)
    $dump = Join-Path $work 'pri.xml'
    Invoke-Tool $makepri @('dump', '/if', $pri, '/of', $dump, '/dt', 'detailed', '/o')
    $index = New-Object System.Xml.XmlDocument
    $index.Load($dump)
    $pictures = Get-ChildItem -LiteralPath (Join-Path $stage 'Assets') -Filter '*.png'
    foreach ($group in ($pictures | Group-Object -Property { $_.Name.Split('.')[0] })) {
        $base = "$($group.Name).png"
        $query = "//*[local-name()='NamedResource' and @name='$base']/*[local-name()='Candidate']"
        $candidates = @($index.SelectNodes($query))
        if ($candidates.Count -ne $group.Count) {
            Select-String -LiteralPath $dump -Pattern $group.Name | Select-Object -First 40 | ForEach-Object { Write-Host $_.Line }
            throw "the index has $($candidates.Count) forms of $base, the folder has $($group.Count) pictures of it"
        }
        Write-Host "index: $base in $($group.Count) forms"
    }
    Copy-Item -LiteralPath $pri -Destination (Join-Path $stage 'resources.pri')

    # The package. MakeAppx checks the manifest as it packs.
    if (Test-Path -LiteralPath $Package) {
        Remove-Item -LiteralPath $Package
    }
    Invoke-Tool $makeappx @('pack', '/d', $stage, '/p', $Package, '/o')

    # Looked at once more, as it was written: it unpacks, and it holds the program
    # and the manifest it was given.
    $check = Join-Path $work 'check'
    Invoke-Tool $makeappx @('unpack', '/p', $Package, '/d', $check, '/o')
    foreach ($name in @('OnEum.exe', 'AppxManifest.xml', 'resources.pri')) {
        if ((Get-Sha256 (Join-Path $check $name)) -ne (Get-Sha256 (Join-Path $stage $name))) {
            throw "$name in the package is not the one that was packed"
        }
    }
    if ((Get-Sha256 (Join-Path $check 'OnEum.exe')) -ne (Get-Sha256 (Join-Path $Layout 'OnEum.exe'))) {
        throw 'OnEum.exe in the package is not the one in the folder'
    }
    if (-not (Test-Path -LiteralPath (Join-Path $check 'AppxBlockMap.xml'))) {
        throw 'the package has no block map'
    }
    if (Test-Path -LiteralPath (Join-Path $check 'AppxSignature.p7x')) {
        throw 'the package is signed, which this script does not do'
    }
    $size = (Get-Item -LiteralPath $Package).Length
    Write-Host "built $Package ($size bytes, not signed)"
    Write-Host "SHA-256 $((Get-Sha256 $Package).ToLower())"
}
finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}
