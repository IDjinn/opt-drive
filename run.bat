@echo off
rem Opt-Drive — inicializacao rapida (Windows)
rem Uso:
rem   run.bat            inicia o app desktop, buildando antes o que faltar
rem   run.bat rebuild    forca recompilar o workspace Rust antes de abrir

setlocal
cd /d "%~dp0"

rem 1) Rust: builda o workspace se o binario do daemon nao existe
if /i "%~1"=="rebuild" goto build
if exist "target\debug\opt-drive-daemon.exe" goto node_deps

:build
where cargo >nul 2>nul || (echo [opt-drive] ERRO: cargo nao encontrado no PATH. Instale o Rust stable. & goto fail)
echo [opt-drive] compilando Rust workspace...
cargo build || goto fail

:node_deps
rem 2) Desktop: instala dependencias npm se faltam
if exist "desktop\node_modules" goto postinstall
echo [opt-drive] instalando dependencias do desktop (npm install)...
pushd desktop
call npm install || (popd & goto fail)
popd

:postinstall
rem 3) Node 24+ pode bloquear os postinstalls de electron/esbuild — completa na mao
if not exist "desktop\node_modules\electron\dist\electron.exe" (
    if exist "desktop\node_modules\electron\install.js" (
        echo [opt-drive] completando instalacao do Electron...
        pushd desktop
        node node_modules\electron\install.js
        popd
    )
)
if not exist "desktop\node_modules\@esbuild\win32-x64\esbuild.exe" (
    if exist "desktop\node_modules\esbuild\install.js" (
        echo [opt-drive] completando instalacao do esbuild...
        pushd desktop
        node node_modules\esbuild\install.js
        popd
    )
)

rem 4) Sobe vite + electron; o electron spawna o daemon de target\debug
echo [opt-drive] abrindo desktop: vite + electron + daemon...
pushd desktop
call npm run dev
popd
goto eof

:fail
echo [opt-drive] ERRO: uma das etapas falhou. Veja a mensagem acima.
exit /b 1

:eof
endlocal
