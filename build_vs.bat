@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul 2>&1
set PATH=C:\Users\joshu\.cargo\bin;C:\Program Files\LLVM\bin;%PATH%
cd /d C:\Users\joshu\projects\servo
python3 mach build --release
