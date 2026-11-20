# 构建检测程序，和 adb、run_probe.bat 一起打成 zip，输出到 android/build/probe_kit/。
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
$tools = Join-Path $sdk 'platform-tools'
$kit = Join-Path $root 'android\build\probe_kit'
$folder = Join-Path $kit 'OpenSplatCaptureProbe'
$zip = Join-Path $kit 'OpenSplatCaptureProbe.zip'

& (Join-Path $root 'android\gradlew.bat') -p (Join-Path $root 'android') assembleProbeDebug
if ($LASTEXITCODE -ne 0) { throw 'Gradle build failed' }

if (Test-Path $kit) { Remove-Item -Recurse -Force $kit }
New-Item -ItemType Directory -Force (Join-Path $folder 'platform-tools') | Out-Null

Copy-Item (Join-Path $root 'android\build\outputs\apk\probe\debug\open_capture-probe-debug.apk') (Join-Path $folder 'open_capture_probe.apk')
Copy-Item (Join-Path $PSScriptRoot 'run_probe.bat') $folder
foreach ($name in 'adb.exe', 'AdbWinApi.dll', 'AdbWinUsbApi.dll', 'libwinpthread-1.dll', 'NOTICE.txt') {
    $file = Join-Path $tools $name
    if (Test-Path $file) { Copy-Item $file (Join-Path $folder 'platform-tools') }
}

Compress-Archive -Path $folder -DestinationPath $zip
Write-Host "已生成 $zip"
