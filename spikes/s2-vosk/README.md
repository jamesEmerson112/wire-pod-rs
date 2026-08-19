# s2-vosk linking

MSVC needs an import library; the wire-pod install dir ships only the DLL.
The official `vosk-win64-0.3.45.zip` (github.com/alphacep/vosk-api releases)
provides `libvosk.lib` — unpack it into `vendor/vosk-win64-0.3.45/` (gitignored;
`build.rs` adds it to the link search path) and put that dir on PATH at runtime
for `libvosk.dll`.
