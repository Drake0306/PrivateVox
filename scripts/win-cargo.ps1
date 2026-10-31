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

& cargo @args
exit $LASTEXITCODE
