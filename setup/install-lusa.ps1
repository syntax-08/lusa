# install-lusa.ps1 — installs the Lusa runtime on this machine (no cargo needed at runtime)
# Usage: install-lusa.cmd   (double-click or run from a terminal)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

# 1) locate lusa.exe (portable package first, then repo release/debug)
$src = Join-Path $root "lusa.exe"
$mode = "portable"
if (-not (Test-Path $src)) {
    $src = Join-Path $root "target\release\lusa.exe"
    $mode = "release"
    if (-not (Test-Path $src)) {
        $src = Join-Path $root "target\debug\lusa.exe"
        $mode = "debug"
    }
}
if (-not (Test-Path $src)) {
    Write-Host "ERROR: lusa.exe not found." -ForegroundColor Red
    Write-Host "Build first:  cargo build --release   (inside $root)"
    exit 1
}

# 2) copy to %LOCALAPPDATA%\Lusa\bin
$bin = Join-Path $env:LOCALAPPDATA "Lusa\bin"
New-Item -ItemType Directory -Force -Path $bin | Out-Null
Copy-Item $src (Join-Path $bin "lusa.exe") -Force

# 3) the `lusalang` wrapper (same binary, package-manager name)
$wrapper = @"
@echo off
"%~dp0lusa.exe" %*
"@
Set-Content -Path (Join-Path $bin "lusalang.cmd") -Value $wrapper -Encoding ASCII

# 4) add to user PATH (idempotent, never truncates)
$binReg = "%LOCALAPPDATA%\Lusa\bin"
$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
$pathVal = [string]$key.GetValue("Path", "", "DoNotExpandEnvironmentNames")
$parts = $pathVal.Split(";") | Where-Object { $_ -ne "" }
if ($parts -notcontains $binReg) {
    $newPath = if ($parts.Count -gt 0) { ($parts + $binReg) -join ";" } else { $binReg }
    if ($newPath.Length -gt 2047) {
        Write-Host "WARNING: user PATH is very long (>2047); install manually." -ForegroundColor Yellow
    } else {
        $key.SetValue("Path", $newPath, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    }
}
$key.Close()

# 5) notify new processes (terminals already open are unaffected)
Add-Type -Namespace Win32 -Name Env -MemberDefinition @"
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
"@
$ptr = [UIntPtr]::Zero
[Win32.Env]::SendMessageTimeout([IntPtr]0xffff, 0x001A, [UIntPtr]::Zero, "Environment", 2, 5000, [ref]$ptr) | Out-Null

Write-Host ""
Write-Host "Lusa installed successfully!" -ForegroundColor Green
Write-Host ("  source : " + $src + "  [" + $mode + "]")
Write-Host ("  target : " + $bin)
Write-Host ""
Write-Host "Commands available (open a NEW TERMINAL for PATH to apply):"
Write-Host "  lusalang version          runtime version (alias: lusa -v)"
Write-Host "  lusalang run main.lusa    run a program"
Write-Host "  lusalang check main.lusa  validate without running"
Write-Host "  lusalang new myproject    create a ready-to-edit project"
Write-Host "  lusalang repl             interactive REPL"
Write-Host "  lusalang where            where the runtime is installed"
