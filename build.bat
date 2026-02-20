@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
set CC=clang-cl.exe
set CXX=clang-cl.exe
set PATH=C:\Users\joshu\.cargo\bin;C:\Users\joshu\AppData\Local\Programs\Python\Python312;C:\Users\joshu\AppData\Local\Programs\Python\Python312\Scripts;C:\Program Files\LLVM\bin;%PATH%
cd /d C:\Users\joshu\projects\servo
python mach build --release --media-stack=dummy 2>&1
