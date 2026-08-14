@echo off
setlocal
REM install.bat - build a Rust binary in release mode and install to %USERPROFILE%\bin.
REM Edit BIN_NAME after cp.

set BIN_NAME=exportsnap
set INSTALL_DIR=%USERPROFILE%\bin

if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"
cargo build --release || exit /b 1
copy /Y "target\release\%BIN_NAME%.exe" "%INSTALL_DIR%\%BIN_NAME%.exe"
echo installed %BIN_NAME% -^> %INSTALL_DIR%\%BIN_NAME%.exe
