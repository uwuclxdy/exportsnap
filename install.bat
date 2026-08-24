@echo off
setlocal
REM install.bat - build a Rust binary in release mode and install to %USERPROFILE%\bin.

set BIN_NAME=exportsnap
set INSTALL_DIR=%USERPROFILE%\bin

REM rusqlite bundles SQLite C, so the build needs a C toolchain: gcc (mingw) for the
REM gnu toolchain, cl or a VS install for msvc, clang for gnullvm. Check what cc would
REM find (cc 1.4.0's per-target chain), fail closed.
where rustc >nul 2>nul || (
  echo install.bat: rustc not found on PATH. Install Rust from https://rustup.rs, then re-run.
  exit /b 1
)
set HOST_TRIPLE=
for /f "tokens=2" %%a in ('rustc -vV 2^>nul ^| findstr /b /c:"host: "') do set HOST_TRIPLE=%%a
if not defined HOST_TRIPLE (
  echo install.bat: rustc is installed but reports no host triple. Run "rustup default stable" to install a toolchain, then re-run.
  exit /b 1
)
if defined CC goto have_c_toolchain
echo %HOST_TRIPLE%| findstr "windows-gnu$" >nul && set NEED_GCC=1
echo %HOST_TRIPLE%| findstr "windows-msvc$" >nul && set NEED_MSVC=1
if defined NEED_GCC (
  where gcc.exe >nul 2>nul && goto have_c_toolchain
  set TOOLCHAIN_FIX=Install mingw gcc and make sure gcc.exe is on PATH, then re-run.
  goto no_c_toolchain
)
set VS_REQ=Microsoft.VisualStudio.Component.VC.Tools.x86.x64
echo %HOST_TRIPLE%| findstr "^aarch64" >nul && set VS_REQ=Microsoft.VisualStudio.Component.VC.Tools.ARM64
echo %HOST_TRIPLE%| findstr "^arm" >nul && set VS_REQ=Microsoft.VisualStudio.Component.VC.Tools.ARM
if defined NEED_MSVC (
  where cl.exe >nul 2>nul && goto have_c_toolchain
  if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" (
    "%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires %VS_REQ% -property installationPath 2>nul | findstr /c:":" >nul && goto have_c_toolchain
  )
  set TOOLCHAIN_FIX=Install Visual Studio Build Tools with the "Desktop development with C++" workload, then re-run.
  goto no_c_toolchain
)
echo %HOST_TRIPLE%| findstr "gnullvm$" >nul && where clang.exe >nul 2>nul && goto have_c_toolchain
set TOOLCHAIN_FIX=Install a C toolchain (mingw gcc, clang, or Visual Studio Build Tools with the C++ workload), then re-run.
goto no_c_toolchain

:no_c_toolchain
echo install.bat: exportsnap bundles SQLite, which needs a C toolchain to build, but none was found.
echo %TOOLCHAIN_FIX%
exit /b 1

:have_c_toolchain
REM A FILE at %INSTALL_DIR% passes "if not exist" silently and the failure lands
REM unnamed at the copy step. "if exist ...\" matches directories only, so name the
REM file collision here, before the expensive build.
if exist "%INSTALL_DIR%\" goto have_install_dir
if exist "%INSTALL_DIR%" (
  echo install.bat: %INSTALL_DIR% is a file. Remove or rename it, then re-run.
  exit /b 1
)
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"
if not exist "%INSTALL_DIR%" (
  echo install.bat: could not create %INSTALL_DIR%
  exit /b 1
)
:have_install_dir
if exist "%INSTALL_DIR%\%BIN_NAME%.exe\" (
  echo install.bat: %INSTALL_DIR%\%BIN_NAME%.exe is a directory. Remove or rename it, then re-run.
  exit /b 1
)
cargo build --release || exit /b 1
copy /Y "target\release\%BIN_NAME%.exe" "%INSTALL_DIR%\%BIN_NAME%.exe" >nul || exit /b 1
echo installed %BIN_NAME% -^> %INSTALL_DIR%\%BIN_NAME%.exe
