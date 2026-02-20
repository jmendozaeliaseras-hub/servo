@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=amd64
set PATH=%USERPROFILE%\.cargo\bin;C:\Program Files\LLVM\bin;%PATH%
cd /d C:\Users\joshu\projects\servo
python mach build --release --media-stack=dummy
