@echo off
chcp 65001 >nul
setlocal EnableDelayedExpansion
cd /d "%~dp0"

rem 安装检测程序、等待检测结束，然后把结果拉回 results 目录。

set "ADB=%~dp0platform-tools\adb.exe"
set "APK=%~dp0open_capture_probe.apk"
set "PKG=io.github.him049.opensplatcapture.probe"
set "WAIT_SECONDS=150"

echo ============================================================
echo  Open Splat Capture 设备检测
echo ============================================================
echo  1. 头显里打开开发者模式和 USB 调试
echo  2. 用数据线把头显连到电脑
echo  3. 头显里弹出"允许 USB 调试"时选择允许
echo ============================================================
echo.

"%ADB%" start-server >nul 2>&1

:find_device
set "SERIAL="
set "COUNT=0"
set "UNAUTHORIZED=0"
for /f "skip=1 tokens=1,2" %%a in ('"%ADB%" devices') do (
    if "%%b"=="device" (
        set /a COUNT+=1
        set "SERIAL=%%a"
    )
    if "%%b"=="unauthorized" set "UNAUTHORIZED=1"
)
if %COUNT%==0 (
    if %UNAUTHORIZED%==1 (
        echo 头显还没有允许 USB 调试，请戴上头显点"允许"……
    ) else (
        echo 没有找到设备，请检查数据线和 USB 调试设置……
    )
    ping -n 4 127.0.0.1 >nul
    goto find_device
)
if %COUNT% GTR 1 (
    echo 连接了多台设备：
    "%ADB%" devices
    set /p "SERIAL=请输入要检测的设备序列号："
)
set "A="%ADB%" -s %SERIAL%"
echo 使用设备 %SERIAL%
echo.

echo 正在安装检测程序……
%A% install -r "%APK%"
if errorlevel 1 (
    echo 安装失败，请把上面的错误信息截图发回来。
    pause
    exit /b 1
)

%A% shell am force-stop %PKG% >nul 2>&1
%A% logcat -c
%A% shell am start -n %PKG%/android.app.NativeActivity >nul
echo.
echo 检测程序已启动。请戴上头显：
echo   - 弹出权限请求时全部选择允许
echo   - 依次按一遍手柄上的各个按键
echo   - 面板显示"检测完成"后就可以摘下头显
echo.

set /a TICKS=%WAIT_SECONDS%/3
for /l %%i in (1,1,%TICKS%) do (
    %A% logcat -d -s OSCProbe:I 2>nul | findstr /c:"PROBE_DONE" >nul
    if not errorlevel 1 goto collect
    ping -n 4 127.0.0.1 >nul
)
echo 等待超时，照样提取已有的结果。

:collect
set "MODEL=device"
for /f "delims=" %%m in ('%A% shell getprop ro.product.model') do set "MODEL=%%m"
set "MODEL=%MODEL: =_%"
for /f %%t in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd_HHmmss"') do set "STAMP=%%t"
set "OUT=%~dp0results\%MODEL%_%STAMP%"
mkdir "%OUT%" >nul 2>&1

%A% pull /sdcard/Android/data/%PKG%/files/probe_report.json "%OUT%\probe_report.json" >nul 2>&1
%A% logcat -d -s OSCProbe:I > "%OUT%\probe_log.txt" 2>&1
%A% logcat -d > "%OUT%\logcat_full.txt" 2>&1
%A% shell getprop > "%OUT%\getprop.txt" 2>&1

echo.
echo ============================================================
echo  检测结果已保存到：
echo  %OUT%
echo  请把这个文件夹打包发回来，谢谢！
echo ============================================================
explorer "%OUT%"
pause
