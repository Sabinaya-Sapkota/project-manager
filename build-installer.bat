@echo off
cd /d "%~dp0"
where cargo >nul 2>nul || (echo Rust is not installed. Get it from https://rustup.rs and the "Desktop development with C++" tools from Visual Studio Build Tools, then run this again. & pause & exit /b 1)
cargo tauri --version >nul 2>nul || cargo install tauri-cli --version "^2" --locked
cargo tauri build
if errorlevel 1 (echo. & echo Build failed - see the errors above. & pause & exit /b 1)
if not exist installer mkdir installer
copy /y src-tauri\target\release\bundle\nsis\*.exe installer\ >nul
echo.
echo Done. Your installer is in the "installer" folder.
explorer installer
pause
