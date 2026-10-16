@echo off
setlocal

for /f "usebackq delims=" %%I in (`wsl.exe -d Ubuntu -- wslpath -a "%CD%"`) do set "PUBLISHER_WSL_CWD=%%I"
if not defined PUBLISHER_WSL_CWD (
  echo Could not translate the current directory for WSL. 1>&2
  exit /b 1
)

for /f "usebackq delims=" %%I in (`wsl.exe -d Ubuntu -- bash -lc "command -v cargo"`) do set "PUBLISHER_WSL_CARGO=%%I"
if not defined PUBLISHER_WSL_CARGO (
  echo Could not find cargo in the WSL login environment. 1>&2
  exit /b 1
)

wsl.exe -d Ubuntu --cd "%PUBLISHER_WSL_CWD%" -- env "PATH=/usr/lib/llvm-18/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" "%PUBLISHER_WSL_CARGO%" xwin %*
exit /b %ERRORLEVEL%
