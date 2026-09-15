[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$UnsignedApk,

    [Parameter(Mandatory = $true)]
    [string]$SignerZip,

    [string]$OutputApk,

    [switch]$Force
)

$ErrorActionPreference = "Stop"

# The platform.pk8 and platform.x509.pem pair is used for Android system signing.

function Resolve-FilePath {
    param([string]$Path, [string]$Description)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description not found: $Path"
    }
    return [System.IO.Path]::GetFullPath((Resolve-Path -LiteralPath $Path).Path)
}

$unsignedPath = Resolve-FilePath -Path $UnsignedApk -Description "Unsigned APK"
$signerZipPath = Resolve-FilePath -Path $SignerZip -Description "Signer ZIP"

if ([string]::IsNullOrWhiteSpace($OutputApk)) {
    $OutputApk = Join-Path ([System.IO.Path]::GetDirectoryName($unsignedPath)) "junnuo3576.apk"
} else {
    $OutputApk = [System.IO.Path]::GetFullPath($OutputApk)
}

if ((Test-Path -LiteralPath $OutputApk -PathType Leaf) -and -not $Force) {
    throw "Output already exists. Choose another path or pass -Force: $OutputApk"
}

$sdkRoots = @()
if ($env:ANDROID_SDK_ROOT) { $sdkRoots += $env:ANDROID_SDK_ROOT }
if ($env:ANDROID_HOME) { $sdkRoots += $env:ANDROID_HOME }
if ($env:LOCALAPPDATA) { $sdkRoots += (Join-Path $env:LOCALAPPDATA "Android\Sdk") }

$apksigner = $null
foreach ($sdkRoot in ($sdkRoots | Select-Object -Unique)) {
    if (-not (Test-Path -LiteralPath $sdkRoot -PathType Container)) { continue }
    $candidate = Get-ChildItem -LiteralPath (Join-Path $sdkRoot "build-tools") `
        -Filter "apksigner.bat" -File -Recurse -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($candidate) {
        $apksigner = $candidate.FullName
        break
    }
}

if (-not $apksigner) {
    throw "Android SDK apksigner.bat was not found. Set ANDROID_SDK_ROOT or install Android build-tools."
}

$javaHomes = @()
if ($env:JAVA_HOME) { $javaHomes += $env:JAVA_HOME }
if ($env:ANDROID_STUDIO_JAVA_HOME) { $javaHomes += $env:ANDROID_STUDIO_JAVA_HOME }
if ($env:ProgramFiles) {
    $javaHomes += (Join-Path $env:ProgramFiles "Android\Android Studio\jbr")
    $javaHomes += Get-ChildItem -LiteralPath (Join-Path $env:ProgramFiles "Java") `
        -Directory -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }
}

$javaHome = $null
foreach ($candidate in ($javaHomes | Select-Object -Unique)) {
    if (Test-Path -LiteralPath (Join-Path $candidate "bin\java.exe") -PathType Leaf) {
        $javaHome = $candidate
        break
    }
}
if (-not $javaHome) {
    throw "A usable Java runtime was not found for apksigner. Set JAVA_HOME to JDK 11 or newer."
}
$env:JAVA_HOME = $javaHome

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("junnuo3576-sign-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory($signerZipPath, $tempRoot)

    $keyPath = Join-Path $tempRoot "platform.pk8"
    $certPath = Join-Path $tempRoot "platform.x509.pem"
    if (-not (Test-Path -LiteralPath $keyPath -PathType Leaf)) {
        throw "Signer ZIP does not contain platform.pk8"
    }
    if (-not (Test-Path -LiteralPath $certPath -PathType Leaf)) {
        throw "Signer ZIP does not contain platform.x509.pem"
    }

    $signedTemp = Join-Path $tempRoot "junnuo3576-signed.apk"
    & $apksigner sign --key $keyPath --cert $certPath --out $signedTemp $unsignedPath
    if ($LASTEXITCODE -ne 0) {
        throw "apksigner sign failed with exit code $LASTEXITCODE"
    }

    & $apksigner verify --verbose --print-certs $signedTemp
    if ($LASTEXITCODE -ne 0) {
        throw "apksigner verification failed with exit code $LASTEXITCODE"
    }

    $outputDirectory = [System.IO.Path]::GetDirectoryName($OutputApk)
    if ($outputDirectory -and -not (Test-Path -LiteralPath $outputDirectory -PathType Container)) {
        New-Item -ItemType Directory -Path $outputDirectory | Out-Null
    }
    Move-Item -LiteralPath $signedTemp -Destination $OutputApk -Force:$Force
    Write-Output "Signed APK: $OutputApk"
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}

