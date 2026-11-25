#Requires -Version 5.1
<#
.SYNOPSIS
    Build the dependency-free "zidoo fixed-48k AudioTrack test" APK with the raw Android
    SDK toolchain (aapt2 + javac + d8 + zipalign + apksigner). No gradle, no AndroidX.

.DESCRIPTION
    Produces zidoo-fixed48-test-v1.0.apk in artifacts/tspi-audio/zidoo-test/ and writes
    build evidence (log, apk info, toolchain, hashes) into
    rt-stack/evidence/calib-chain-20261121/zidoo-apk48/build-record/.

    The signing keystore is a throw-away test key generated on first run under
    scratch/apk48/keystore/ (scratch/ is git-ignored; never commit the keystore).

.EXAMPLE
    pwsh -File build.ps1
#>
[CmdletBinding()]
param(
    [string]$SdkRoot = '',
    [string]$BuildTools = '35.0.0',
    [string]$Platform = 'android-35',
    [string]$KeystorePath = '',
    [string]$KeystorePass = 'tspi48test',
    [string]$KeyAlias = 'zidoo-fixed48',
    [switch]$NoArtifactCopy
)

$ErrorActionPreference = 'Stop'
$NL = [Environment]::NewLine

$here = $PSScriptRoot
$repoRoot = (Resolve-Path (Join-Path $here '..\..\..')).Path
$srcRoot = Join-Path $here 'src'

if (-not $SdkRoot) {
    if ($env:ANDROID_SDK_ROOT) { $SdkRoot = $env:ANDROID_SDK_ROOT }
    elseif ($env:ANDROID_HOME) { $SdkRoot = $env:ANDROID_HOME }
    else { $SdkRoot = Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
}
if (-not $KeystorePath) {
    $KeystorePath = Join-Path $repoRoot 'scratch\apk48\keystore\zidoo-fixed48.jks'
}

$bt = Join-Path $SdkRoot ('build-tools\' + $BuildTools)
$aapt2 = Join-Path $bt 'aapt2.exe'
$d8 = Join-Path $bt 'd8.bat'
$zipalign = Join-Path $bt 'zipalign.exe'
$apksigner = Join-Path $bt 'apksigner.bat'
$androidJar = Join-Path $SdkRoot ('platforms\' + $Platform + '\android.jar')

$scratchDir = Join-Path $repoRoot 'scratch\apk48'
$workDir = Join-Path $scratchDir 'build'
$classesDir = Join-Path $workDir 'classes'
$dexDir = Join-Path $workDir 'dex'
$distDir = Join-Path $repoRoot 'artifacts\tspi-audio\zidoo-test'
$evidenceRoot = Join-Path $repoRoot 'rt-stack\evidence\calib-chain-20261121\zidoo-apk48'
# NOTE: do not name this directory "build": the repo .gitignore ignores **/build/.
$evidenceBuild = Join-Path $evidenceRoot 'build-record'
$buildLog = Join-Path $evidenceBuild 'build.log'
$apkName = 'zidoo-fixed48-test-v1.0.apk'

# ---- paths / tools sanity -------------------------------------------------
$javac = (Get-Command javac -ErrorAction SilentlyContinue)
$jar = (Get-Command jar -ErrorAction SilentlyContinue)
$keytool = (Get-Command keytool -ErrorAction SilentlyContinue)
$java = (Get-Command java -ErrorAction SilentlyContinue)
if (-not $javac) { throw 'javac not found on PATH (install JDK 17/21)' }
if (-not $jar) { throw 'jar not found on PATH (install JDK 17/21)' }
if (-not $keytool) { throw 'keytool not found on PATH (install JDK 17/21)' }
if (-not $java) { throw 'java not found on PATH (needed by d8.bat/apksigner.bat)' }
foreach ($tool in @($aapt2, $d8, $zipalign, $apksigner, $androidJar)) {
    if (-not (Test-Path $tool)) { throw ('missing Android SDK tool: ' + $tool) }
}
if (-not (Test-Path (Join-Path $srcRoot 'AndroidManifest.xml'))) {
    throw ('missing source manifest under ' + $srcRoot)
}

New-Item -ItemType Directory -Force -Path $scratchDir, $workDir, $evidenceBuild, $distDir | Out-Null
if (Test-Path $buildLog) { Remove-Item $buildLog -Force }

function Write-Log([string]$text) {
    if ($null -eq $text) { $text = '' }
    Add-Content -Path $buildLog -Value $text -Encoding utf8
    Write-Host $text
}

function Protect-Text([string]$text) {
    if ($text -and $KeystorePass) { return $text.Replace($KeystorePass, '***') }
    return $text
}

function Capture-Text {
    param([string]$Exe, [string[]]$Arguments)
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        return ((& $Exe @Arguments 2>&1 | Out-String).Trim())
    } finally {
        $ErrorActionPreference = $oldEap
    }
}

function Invoke-Logged {
    param(
        [string]$Name,
        [string]$Exe,
        [string[]]$Arguments,
        [switch]$AllowFailure
    )
    Write-Log ''
    Write-Log ('>>> ' + $Name)
    Write-Log ('    ' + (Protect-Text ($Exe + ' ' + ($Arguments -join ' '))))
    # PowerShell 5.1 turns redirected native stderr into ErrorRecords; with EAP=Stop that
    # would abort on a harmless javac/d8 warning, so relax EAP just for the invocation.
    $oldEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = @(& $Exe @Arguments 2>&1 | ForEach-Object { $_.ToString() })
        $exit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $oldEap
    }
    foreach ($line in $output) { Write-Log $line }
    Write-Log ('<<< ' + $Name + ' exit=' + $exit)
    if ($exit -ne 0 -and -not $AllowFailure) {
        throw ($Name + ' failed with exit code ' + $exit)
    }
}

Write-Log ('== zidoo-fixed48-test build ' + (Get-Date).ToString('o') + ' ==')
Write-Log ('repoRoot   : ' + $repoRoot)
Write-Log ('sdkRoot    : ' + $SdkRoot)
Write-Log ('buildTools : ' + $bt)
Write-Log ('androidJar : ' + $androidJar)

# ---- clean work dir -------------------------------------------------------
if (Test-Path $workDir) { Remove-Item $workDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $classesDir, $dexDir | Out-Null

# ---- 1. javac -------------------------------------------------------------
$javaSources = @(Get-ChildItem (Join-Path $srcRoot 'java') -Recurse -Filter '*.java' |
    Sort-Object FullName | ForEach-Object { $_.FullName })
if ($javaSources.Count -eq 0) { throw 'no java sources found' }

$javacArgs = @('-encoding', 'UTF-8', '-source', '8', '-target', '8', '-Xlint:-options',
    '-bootclasspath', $androidJar, '-d', $classesDir) + $javaSources
Invoke-Logged -Name 'javac' -Exe $javac.Source -Arguments $javacArgs

# ---- 2. jar + d8 ----------------------------------------------------------
$classesJar = Join-Path $workDir 'classes.jar'
Invoke-Logged -Name 'jar' -Exe $jar.Source -Arguments @('cf', $classesJar, '-C', $classesDir, '.')
Invoke-Logged -Name 'd8' -Exe $d8 -Arguments @('--release', '--min-api', '30',
    '--lib', $androidJar, '--output', $dexDir, $classesJar)

$dexFile = Join-Path $dexDir 'classes.dex'
if (-not (Test-Path $dexFile)) { throw ('d8 produced no classes.dex in ' + $dexDir) }

# ---- 3. aapt2 link + merge dex -------------------------------------------
$unsignedApk = Join-Path $workDir 'unsigned.apk'
Invoke-Logged -Name 'aapt2 link' -Exe $aapt2 -Arguments @('link', '-o', $unsignedApk,
    '-I', $androidJar, '--manifest', (Join-Path $srcRoot 'AndroidManifest.xml'))

# jar.exe can update an existing zip; run inside workDir so the entry lands as /classes.dex
Copy-Item -Path $dexFile -Destination (Join-Path $workDir 'classes.dex') -Force
Push-Location $workDir
try {
    Invoke-Logged -Name 'jar update classes.dex' -Exe $jar.Source -Arguments @('uf', $unsignedApk, 'classes.dex')
} finally {
    Pop-Location
}
Write-Log ''
Write-Log ('merged classes.dex (' + (Get-Item $dexFile).Length + ' bytes) into unsigned.apk')

# ---- 4. zipalign ----------------------------------------------------------
$alignedApk = Join-Path $workDir 'aligned.apk'
Invoke-Logged -Name 'zipalign' -Exe $zipalign -Arguments @('-f', '-p', '4', $unsignedApk, $alignedApk)

# ---- 5. keystore ----------------------------------------------------------
$keystoreDir = Split-Path -Parent $KeystorePath
if (-not (Test-Path $keystoreDir)) { New-Item -ItemType Directory -Force -Path $keystoreDir | Out-Null }
if (-not (Test-Path $KeystorePath)) {
    Write-Log ''
    Write-Log ('generating throw-away test keystore: ' + $KeystorePath)
    $keytoolArgs = @('-genkeypair', '-keystore', $KeystorePath, '-storetype', 'PKCS12',
        '-alias', $KeyAlias, '-keyalg', 'RSA', '-keysize', '2048', '-validity', '36500',
        '-dname', 'CN=TSPI Fixed48 Test, OU=TSPI, O=TSPI, L=NA, ST=NA, C=CN',
        '-storepass', $KeystorePass, '-keypass', $KeystorePass, '-noprompt')
    Invoke-Logged -Name 'keytool -genkeypair' -Exe $keytool.Source -Arguments $keytoolArgs
} else {
    Write-Log ('reusing existing test keystore: ' + $KeystorePath)
}
$keystoreSha = (Get-FileHash -Algorithm SHA256 $KeystorePath).Hash.ToLower()

# ---- 6. sign --------------------------------------------------------------
$signedApk = Join-Path $workDir $apkName
Invoke-Logged -Name 'apksigner sign' -Exe $apksigner -Arguments @('sign',
    '--ks', $KeystorePath, '--ks-key-alias', $KeyAlias,
    '--ks-pass', ('pass:' + $KeystorePass), '--key-pass', ('pass:' + $KeystorePass),
    '--min-sdk-version', '30',
    '--v1-signing-enabled', 'false', '--v2-signing-enabled', 'true', '--v3-signing-enabled', 'false',
    '--out', $signedApk, $alignedApk)

# ---- 7. verify ------------------------------------------------------------
$verifyText = Capture-Text $apksigner @('verify', '--verbose', '--print-certs', $signedApk)
$verifyExit = $LASTEXITCODE
Write-Log ''
Write-Log ('>>> apksigner verify')
foreach ($line in ($verifyText -split [Environment]::NewLine)) { Write-Log $line }
Write-Log ('<<< apksigner verify exit=' + $verifyExit)
if ($verifyExit -ne 0) { throw 'apksigner verify failed' }

$badgingText = Capture-Text $aapt2 @('dump', 'badging', $signedApk)
$badgingExit = $LASTEXITCODE
Write-Log ''
Write-Log ('>>> aapt2 dump badging')
foreach ($line in ($badgingText -split [Environment]::NewLine)) { Write-Log $line }
Write-Log ('<<< aapt2 dump badging exit=' + $badgingExit)
if ($badgingExit -ne 0) { throw 'aapt2 dump badging failed' }

$apkHash = (Get-FileHash -Algorithm SHA256 $signedApk).Hash.ToLower()
$apkSize = (Get-Item $signedApk).Length

# zip entry listing (jar tvf works on any zip/apk)
$entries = @((& $jar.Source tvf $signedApk 2>&1 | Out-String).Trim() -split [Environment]::NewLine)

# ---- 8. artifact ----------------------------------------------------------
$artifacts = @()
if (-not $NoArtifactCopy) {
    $artifactApk = Join-Path $distDir $apkName
    Copy-Item -Path $signedApk -Destination $artifactApk -Force
    $artHash = (Get-FileHash -Algorithm SHA256 $artifactApk).Hash.ToLower()
    if ($artHash -ne $apkHash) { throw 'artifact copy hash mismatch' }
    Set-Content -Path (Join-Path $distDir 'SHA256SUMS.txt') -Encoding ascii -Value ($apkHash + '  ' + $apkName + $NL)
    $artifacts += $artifactApk
    Write-Log ''
    Write-Log ('artifact: ' + $artifactApk)
} else {
    Write-Log ''
    Write-Log 'artifact copy skipped (-NoArtifactCopy)'
}

# ---- 9. evidence ----------------------------------------------------------
$apkInfo = @()
$apkInfo += ('APK info - zidoo-fixed48-test v1.0')
$apkInfo += ('builtAt   : ' + (Get-Date).ToString('o'))
$apkInfo += ('file      : ' + $signedApk)
$apkInfo += ('size      : ' + $apkSize + ' bytes')
$apkInfo += ('sha256    : ' + $apkHash)
$apkInfo += ('package   : com.tspi.fixed48test')
$apkInfo += ('minSdk    : 30   targetSdk : 30   versionCode : 1   versionName : 1.0')
$apkInfo += ('keystore  : ' + $KeystorePath)
$apkInfo += ('keystoreSha256 : ' + $keystoreSha)
$apkInfo += ''
$apkInfo += '--- zip entries ---'
$apkInfo += $entries
$apkInfo += ''
$apkInfo += '--- aapt2 dump badging ---'
$apkInfo += $badgingText
$apkInfo += ''
$apkInfo += '--- apksigner verify --verbose --print-certs ---'
$apkInfo += $verifyText
Set-Content -Path (Join-Path $evidenceBuild 'apk-info.txt') -Encoding utf8 -Value ($apkInfo -join $NL)

$certText = Capture-Text $keytool.Source @('-list', '-v', '-keystore', $KeystorePath, '-storepass', $KeystorePass, '-alias', $KeyAlias)
if ($LASTEXITCODE -eq 0) {
    Set-Content -Path (Join-Path $evidenceBuild 'signing-cert.txt') -Encoding utf8 -Value $certText
}

# toolchain record
$tc = @()
$tc += ('toolchain - zidoo-fixed48-test')
$tc += ('generated : ' + (Get-Date).ToString('o'))
$tc += ('os        : ' + [System.Environment]::OSVersion.VersionString)
$tc += ('sdkRoot   : ' + $SdkRoot)
$tc += ('buildTools: ' + $bt)
$tc += ('platform  : ' + $Platform)
$tc += ('javac     : ' + (Capture-Text $javac.Source @('-version')))
$tc += ('java      : ' + (Capture-Text $java.Source @('-version')))
$tc += ('d8        : ' + (Capture-Text $d8 @('--version')))
$tc += ('aapt2     : ' + (Capture-Text $aapt2 @('version')))
$tc += ('apksigner : ' + (Capture-Text $apksigner @('--version')))
$tc += ('zipalign  : ' + (Capture-Text $zipalign @()))
Set-Content -Path (Join-Path $evidenceBuild 'toolchain.txt') -Encoding utf8 -Value ($tc -join $NL)

# source hashes (reproducibility manifest)
$srcFiles = @(Get-ChildItem -Path $here -Recurse -File -Force |
    Where-Object { $_.FullName -notmatch '\\build\\' } | Sort-Object FullName)
$srcLines = @()
foreach ($f in $srcFiles) {
    $rel = $f.FullName.Substring($here.Length + 1).Replace('\', '/')
    $srcLines += ((Get-FileHash -Algorithm SHA256 $f.FullName).Hash.ToLower() + '  ' + $rel)
}
Set-Content -Path (Join-Path $evidenceBuild 'source-sha256.txt') -Encoding utf8 -Value ($srcLines -join $NL)

Set-Content -Path (Join-Path $evidenceBuild 'apk-sha256.txt') -Encoding utf8 -Value (
    ($apkHash + '  ' + $apkName + $NL + 'size: ' + $apkSize + ' bytes' + $NL +
     'artifact: ' + (Join-Path $distDir $apkName) + $NL +
     'certSha256: see apk-info.txt (apksigner verify --print-certs)') )

$summary = [ordered]@{
    apk = $apkName
    apkPath = (Join-Path $distDir $apkName)
    sha256 = $apkHash
    sizeBytes = $apkSize
    package = 'com.tspi.fixed48test'
    versionName = '1.0'
    versionCode = 1
    minSdk = 30
    targetSdk = 30
    builtAt = (Get-Date).ToString('o')
    keystore = $KeystorePath
    keystoreSha256 = $keystoreSha
    verificationStatus = 'NOT_RUN (APK built, not installed on Zidoo in this session)'
    sourceDir = $here
}
Set-Content -Path (Join-Path $evidenceBuild 'build-summary.json') -Encoding utf8 -Value (
    ($summary | ConvertTo-Json -Depth 4))

Write-Log ''
Write-Log ('BUILD OK  ' + $apkName)
Write-Log ('  sha256 : ' + $apkHash)
Write-Log ('  size   : ' + $apkSize + ' bytes')
Write-Log ('  apk    : ' + (Join-Path $distDir $apkName))
Write-Log ('  log    : ' + $buildLog)

Write-Host ''
Write-Host ('APK  : ' + (Join-Path $distDir $apkName))
Write-Host ('SHA  : ' + $apkHash)
Write-Host ('SIZE : ' + $apkSize + ' bytes')
