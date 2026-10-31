# Runs a cargo command inside the environment the Windows build needs (mirrors .github/workflows/release.yml):
#   - the MSVC developer environment (whisper.cpp's nested vulkan-shaders-gen CMake project needs cl.exe on PATH)
#   - Ninja as the CMake generator (the Visual Studio generator cannot configure that nested project)
#   - a short target dir, because MSVC refuses object paths over 250 characters
#   - VULKAN_SDK from the LunarG install and LIBCLANG_PATH from the LLVM install (bindgen) when not already set
# One-time installs on a fresh PC: `winget install KhronosGroup.VulkanSDK LLVM.LLVM`, Visual Studio Build Tools with the
# C++ workload, cmake and ninja on PATH, `cargo install tauri-cli --version "^2" --locked`.
# Usage: pwsh scripts/win-cargo.ps1 test -p privatevox-core --lib
#        pwsh scripts/win-cargo.ps1 tauri dev
$ErrorActionPreference = 'Stop'

# Put this shell, cargo and the app it starts into one Windows job that dies with this shell. The Tauri CLI uses this
# script as its cargo runner and stops a dev session by killing the runner process only; without the job, cargo and
# PrivateVox would outlive it (and the next build would fail with "failed to remove privatevox.exe: Access is denied").
try {
    Add-Type -Name Job -Namespace PvxWin -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)]
public static extern System.IntPtr CreateJobObjectW(System.IntPtr attrs, string name);
[System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)]
public static extern bool SetInformationJobObject(System.IntPtr job, int infoClass, System.IntPtr info, uint size);
[System.Runtime.InteropServices.DllImport("kernel32.dll", SetLastError = true)]
public static extern bool AssignProcessToJobObject(System.IntPtr job, System.IntPtr process);
[System.Runtime.InteropServices.DllImport("kernel32.dll")]
public static extern System.IntPtr GetCurrentProcess();
'@
    $job = [PvxWin.Job]::CreateJobObjectW([IntPtr]::Zero, $null)
    if ($job -ne [IntPtr]::Zero) {
        # JOBOBJECT_EXTENDED_LIMIT_INFORMATION (144 bytes on x64) with BasicLimitInformation.LimitFlags at offset 16
        # = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE (0x2000); JobObjectExtendedLimitInformation = 9.
        $size = 144
        $buf = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($size)
        try {
            for ($i = 0; $i -lt $size; $i++) { [System.Runtime.InteropServices.Marshal]::WriteByte($buf, $i, 0) }
            [System.Runtime.InteropServices.Marshal]::WriteInt32($buf, 16, 0x2000)
            if ([PvxWin.Job]::SetInformationJobObject($job, 9, $buf, $size)) {
                [void][PvxWin.Job]::AssignProcessToJobObject($job, [PvxWin.Job]::GetCurrentProcess())
                $script:PvxJobHandle = $job  # keep the handle alive for the life of this shell
            }
        } finally { [System.Runtime.InteropServices.Marshal]::FreeHGlobal($buf) }
    }
} catch { Write-Warning "win-cargo: could not set up the process job ($_); a stopped dev session may leave the app running" }

if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'D:\t' }
if (-not (Test-Path (Split-Path $env:CARGO_TARGET_DIR -Qualifier))) { $env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'pvx-target' }
if (-not $env:CMAKE_GENERATOR) { $env:CMAKE_GENERATOR = 'Ninja' }

if (-not $env:VULKAN_SDK) {
    $sdk = Get-ChildItem 'C:\VulkanSDK' -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1
    if ($sdk) { $env:VULKAN_SDK = $sdk.FullName }
}
if ($env:VULKAN_SDK) { $env:PATH = "$env:VULKAN_SDK\Bin;$env:PATH" }

if (-not $env:LIBCLANG_PATH) {
    $llvm = Join-Path $env:ProgramFiles 'LLVM\bin'
    if (Test-Path (Join-Path $llvm 'libclang.dll')) { $env:LIBCLANG_PATH = $llvm }
}

# The cmake crate reads CMAKE as the executable; a machine that set it to the bin directory gets "Access is denied".
if ($env:CMAKE -and (Test-Path (Join-Path $env:CMAKE 'cmake.exe'))) { $env:CMAKE = Join-Path $env:CMAKE 'cmake.exe' }

# Import the MSVC developer environment (vcvars64) into this process.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $vswhere) {
    $vsdir = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $vcvars = Join-Path $vsdir 'VC\Auxiliary\Build\vcvars64.bat'
    if ($vsdir -and (Test-Path $vcvars)) {
        cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
            $i = $_.IndexOf('=')
            if ($i -gt 0) { [Environment]::SetEnvironmentVariable($_.Substring(0, $i), $_.Substring($i + 1), 'Process') }
        }
    } else {
        Write-Warning 'win-cargo: no Visual Studio with the C++ tools found; building without the developer environment'
    }
} else {
    Write-Warning 'win-cargo: vswhere.exe not found; building without the developer environment'
}

# A dev run or build replaces privatevox.exe, which Windows refuses while a copy from this target directory is still
# running (a leftover of an earlier dev session). Stop that copy first, and only that copy: an installed PrivateVox
# lives elsewhere and is never touched.
if ($args.Count -gt 0 -and $args[0] -in @('run', 'build', 'tauri')) {
    $targetRoot = [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR).TrimEnd('\')
    foreach ($proc in (Get-Process -Name privatevox, privatevox-worker -ErrorAction SilentlyContinue)) {
        $path = try { $proc.Path } catch { $null }
        if ($path -and $path.StartsWith("$targetRoot\", [System.StringComparison]::OrdinalIgnoreCase)) {
            Write-Host "win-cargo: stopping $($proc.Name) (pid $($proc.Id)) left from an earlier dev session so the build can replace it"
            Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        }
    }
}

# Run cargo as a child and watch our own parent while it runs. The Tauri CLI stops a dev session by killing its runner
# (the cmd shim); when that parent disappears this shell exits, the job closes, and cargo and the app go with it.
$parentId = try { (Get-CimInstance Win32_Process -Filter "ProcessId = $PID" -ErrorAction Stop).ParentProcessId } catch { 0 }
$psi = [System.Diagnostics.ProcessStartInfo]::new('cargo')
$psi.UseShellExecute = $false
foreach ($a in $args) { [void]$psi.ArgumentList.Add($a) }
$cargo = [System.Diagnostics.Process]::Start($psi)
while (-not $cargo.HasExited) {
    Start-Sleep -Milliseconds 300
    if ($parentId -and -not (Get-Process -Id $parentId -ErrorAction SilentlyContinue)) {
        exit 1
    }
}
exit $cargo.ExitCode
